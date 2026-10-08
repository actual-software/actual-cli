//! `actual session summary`: one sentence on what Actual AI did in a coding
//! session, for a Claude Code `Stop` hook.
//!
//! # Design
//!
//! **One sentence, built from clauses.** Iteration 1 has one clause, "added X
//! of Y ADRs to context". Later clauses (gaps found, tokens and turns) join the
//! same line, and [`sentence`] is the one place that joins them, so a new
//! clause never changes how the others read.
//!
//! **X and Y.** X is the decisions [`super::brief_ledger`] recorded as added to
//! the context of this session's agents. Y is the decisions the rule set can
//! add to context at all, [`ScopeIndex::decision_keys`]. Both use the key
//! `rules brief` groups by, and X counts only recorded decisions that are still
//! in Y, so a rule set edited mid-session can lower X but never lift it past Y.
//!
//! **Shown when it changes.** A plugin cannot speak once at the end of a
//! session, so the hook speaks at the end of a response, and only when X or Y
//! moved since the user last saw them. The record's `last_reported` holds what
//! they saw, and it is updated under the record's lock before the line is
//! printed. A run that cannot take the lock prints nothing, so two runs at once
//! report a change once.
//!
//! **Keyed on what it is given.** The rules directory is `--rules-dir`, else
//! `<repo>/.actual/rules` with the repository from `--repo` or the working
//! directory, which is how `impl-check` resolves it in the same hook. The
//! envelope's `cwd` follows the agent's `cd`, so [`StopEnvelope`] has no such
//! field.
//!
//! **Fail open under `--claude-hook`.** An unreadable envelope, no session, a
//! rule set that cannot be read, nothing new to say: each prints nothing and
//! exits 0. There is no network, runner or model call, and a cold index is
//! built on the spot, as `rules brief` builds it.
//!
//! [`ScopeIndex::decision_keys`]: crate::rules::scope::ScopeIndex::decision_keys

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::cli::args::{SessionAction, SessionArgs, SessionSummaryArgs};
use crate::cli::commands::brief_ledger::{self, BriefLedger, LedgerKey, Reported};
use crate::cli::commands::rules_brief;
use crate::error::ActualError;
use crate::rules::scope;

/// The one field this command reads from a `Stop` envelope. Every other
/// field is ignored, so a newer envelope still deserializes.
#[derive(Debug, Deserialize, Default)]
#[serde(default)]
struct StopEnvelope {
    session_id: Option<String>,
}

/// `--json`: the counts behind the sentence, and the decisions X counted.
#[derive(Serialize)]
struct SummaryJson<'a> {
    x: usize,
    y: usize,
    decisions: Vec<&'a str>,
}

pub fn exec(args: &SessionArgs) -> Result<(), ActualError> {
    match &args.action {
        SessionAction::Summary(summary) => exec_summary(summary),
    }
}

fn exec_summary(args: &SessionSummaryArgs) -> Result<(), ActualError> {
    if args.claude_hook {
        emit(hook_line(&rules_brief::read_stdin(), args));
        // Always `Ok`: see the fail-open note in the module docs.
        return Ok(());
    }
    let Some(session_id) = args.session.as_deref() else {
        return Err(ActualError::ConfigError(
            "session summary needs --session, or --claude-hook with an envelope on stdin"
                .to_string(),
        ));
    };
    emit(direct_output(session_id, args)?);
    Ok(())
}

/// Print the line, or nothing. Split out so the decision to stay silent is
/// testable without a process boundary.
fn emit(line: Option<String>) {
    if let Some(line) = line {
        println!("{line}");
    }
}

/// The hook's line for one `Stop` envelope, or `None` for silence. Every
/// failure is `None`, so nothing here can become a non-zero exit.
fn hook_line(raw: &str, args: &SessionSummaryArgs) -> Option<String> {
    let envelope: StopEnvelope = serde_json::from_str(raw).ok()?;
    let session_id = envelope.session_id.filter(|id| !id.is_empty())?;
    let (root, rules_dir) = locate(args);
    let dir = brief_ledger::ledger_dir()?;
    let key_dir = rules_brief::resolve(&rules_dir);
    let key = LedgerKey {
        session_id: &session_id,
        rules_dir: &key_dir,
    };

    // Most responses have nothing new to say, so that is settled before the
    // rule set is read or the lock is taken.
    let recorded = brief_ledger::load(&dir, &key);
    if recorded.briefed.is_empty() {
        return None;
    }
    let decisions = rule_set_decisions(&root, &rules_dir).ok()?;
    due(&recorded, &decisions)?;

    let mut shown = None;
    brief_ledger::update(&dir, &key, |ledger| {
        shown = due(ledger, &decisions);
        if shown.is_some() {
            ledger.last_reported = shown;
        }
    });
    sentence(&[added_clause(shown?)])
}

/// Direct mode: the sentence for `session_id` as it stands, or its counts as
/// JSON. It only reads, so a debugging run never changes what the hook says
/// next.
fn direct_output(
    session_id: &str,
    args: &SessionSummaryArgs,
) -> Result<Option<String>, ActualError> {
    let (root, rules_dir) = locate(args);
    let decisions = rule_set_decisions(&root, &rules_dir)?;
    let recorded = brief_ledger::ledger_dir()
        .map(|dir| {
            brief_ledger::load(
                &dir,
                &LedgerKey {
                    session_id,
                    rules_dir: &rules_brief::resolve(&rules_dir),
                },
            )
        })
        .unwrap_or_default();

    if args.json {
        let counted: Vec<&str> = added(&recorded, &decisions).map(String::as_str).collect();
        let json = serde_json::to_string_pretty(&SummaryJson {
            x: counted.len(),
            y: decisions.len(),
            decisions: counted,
        })
        .expect("session summary is serializable — this is a programmer error");
        return Ok(Some(json));
    }
    Ok(sentence(&[added_clause(counts(&recorded, &decisions))]))
}

/// The repository root and the rules directory, resolved the way `impl-check`
/// resolves them beside this command in the same hook.
fn locate(args: &SessionSummaryArgs) -> (PathBuf, PathBuf) {
    let root = args
        .repo
        .clone()
        .unwrap_or_else(crate::cli::commands::sync::resolve_cwd);
    let rules_dir = args
        .rules_dir
        .clone()
        .unwrap_or_else(|| crate::rules::rules_dir(&root));
    (root, rules_dir)
}

/// The decisions the rule set can add to context: Y's members.
fn rule_set_decisions(root: &Path, rules_dir: &Path) -> Result<BTreeSet<String>, ActualError> {
    Ok(scope::resolve_in(rules_dir, root, false)?
        .index
        .decision_keys())
}

/// The recorded decisions the rule set still holds: X's members.
fn added<'a>(
    recorded: &'a BriefLedger,
    decisions: &'a BTreeSet<String>,
) -> impl Iterator<Item = &'a String> {
    recorded.briefed.intersection(decisions)
}

/// X and Y for one record against one rule set.
fn counts(recorded: &BriefLedger, decisions: &BTreeSet<String>) -> Reported {
    Reported {
        added: added(recorded, decisions).count(),
        available: decisions.len(),
    }
}

/// The counts to show, when they are worth showing: something was added, and
/// it is not what the user last saw.
fn due(recorded: &BriefLedger, decisions: &BTreeSet<String>) -> Option<Reported> {
    let now = counts(recorded, decisions);
    (now.added > 0 && recorded.last_reported != Some(now)).then_some(now)
}

/// "added 3 of 12 ADRs to context", with "ADR" singular when Y is 1.
fn added_clause(counts: Reported) -> String {
    let noun = if counts.available == 1 { "ADR" } else { "ADRs" };
    format!(
        "added {} of {} {noun} to context",
        counts.added, counts.available
    )
}

/// Join the clauses that have something to say into the summary sentence, or
/// `None` when none does: one reads "A", two "A and B", more "A, B, and C".
fn sentence(clauses: &[String]) -> Option<String> {
    let joined = match clauses {
        [] => return None,
        [only] => only.clone(),
        [first, second] => format!("{first} and {second}"),
        [rest @ .., last] => format!("{}, and {last}", rest.join(", ")),
    };
    Some(format!("Actual AI: {joined} this session."))
}

#[cfg(test)]
mod tests {
    use super::*;

    use tempfile::{tempdir, TempDir};

    use crate::testutil::{EnvGuard, ENV_MUTEX};

    const ALGORITHM: &str =
        "# Adopt RS256: Algorithm\n\nThese rules are ALWAYS ACTIVE for signing.\n\n### Rules\n\n- **R-A-001** MUST: sign with RS256.\n";
    const EXPIRY: &str =
        "# Adopt RS256: Expiry\n\nThese rules are ALWAYS ACTIVE for signing.\n\n### Rules\n\n- **R-A-002** MUST NOT: issue a token without an expiry.\n";
    const PINNING: &str =
        "# Pin Providers\n\nThese rules are ALWAYS ACTIVE for Terraform.\n\n### Rules\n\n- **R-B-001** MUST: pin every provider.\n";
    const ADVICE: &str =
        "# Advise Caching: Reads\n\nThese rules are ALWAYS ACTIVE for reads.\n\n### Rules\n\n- **R-C-001** SHOULD: cache reads.\n";

    /// Two decisions that can be added to context, "Adopt RS256" (two
    /// documents) and "provider-pinning" (a title naming no decision), and
    /// one that states only advice and never can.
    fn repo() -> TempDir {
        repo_with(&[
            ("signing-algorithm.md", ALGORITHM),
            ("signing-expiry.md", EXPIRY),
            ("provider-pinning.md", PINNING),
            ("caching-reads.md", ADVICE),
        ])
    }

    fn repo_with(files: &[(&str, &str)]) -> TempDir {
        let root = tempdir().unwrap();
        let dir = crate::rules::rules_dir(root.path());
        std::fs::create_dir_all(&dir).unwrap();
        for (name, text) in files {
            std::fs::write(dir.join(name), text).unwrap();
        }
        root
    }

    fn hook_args(root: &Path) -> SessionSummaryArgs {
        SessionSummaryArgs {
            claude_hook: true,
            session: None,
            repo: Some(root.to_path_buf()),
            rules_dir: None,
            json: false,
        }
    }

    fn direct_args(root: &Path) -> SessionSummaryArgs {
        SessionSummaryArgs {
            claude_hook: false,
            session: Some("s1".to_string()),
            ..hook_args(root)
        }
    }

    /// A `Stop` envelope as Claude Code sends it. Its `cwd` names somewhere
    /// else on purpose: the summary must not read it.
    fn stop(session_id: &str) -> String {
        serde_json::json!({
            "session_id": session_id,
            "transcript_path": "/tmp/transcript.jsonl",
            "cwd": "/somewhere/else",
            "hook_event_name": "Stop",
            "stop_hook_active": false,
        })
        .to_string()
    }

    /// The key `rules brief` writes the record under for `root`'s rule set.
    fn key_dir(root: &Path) -> PathBuf {
        rules_brief::resolve(&crate::rules::rules_dir(root))
    }

    /// Record `decisions` as briefed in session `s1`, as `rules brief` does.
    fn brief(root: &Path, decisions: &[&str]) {
        let dir = brief_ledger::ledger_dir().unwrap();
        let rules_dir = key_dir(root);
        let key = LedgerKey {
            session_id: "s1",
            rules_dir: &rules_dir,
        };
        brief_ledger::record(&dir, &key, decisions.iter().copied());
    }

    fn recorded(root: &Path) -> BriefLedger {
        let rules_dir = key_dir(root);
        brief_ledger::load(
            &brief_ledger::ledger_dir().unwrap(),
            &LedgerKey {
                session_id: "s1",
                rules_dir: &rules_dir,
            },
        )
    }

    /// Run `body` with the config directory pointed at a scratch one, so no
    /// test reads or writes the machine's own session records.
    fn with_scratch_config(body: impl FnOnce()) {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _dir = EnvGuard::set("ACTUAL_CONFIG_DIR", home.path().to_str().unwrap());
        let _file = EnvGuard::remove("ACTUAL_CONFIG");
        body();
    }

    // ── the sentence ─────────────────────────────────────────────────────

    #[test]
    fn test_sentence_joins_its_clauses() {
        let clauses = |names: &[&str]| names.iter().map(|c| c.to_string()).collect::<Vec<_>>();
        assert_eq!(sentence(&[]), None);
        assert_eq!(
            sentence(&clauses(&["added 3 of 12 ADRs to context"])).as_deref(),
            Some("Actual AI: added 3 of 12 ADRs to context this session.")
        );
        assert_eq!(
            sentence(&clauses(&["added 3 of 12 ADRs to context", "found 2 gaps"])).as_deref(),
            Some("Actual AI: added 3 of 12 ADRs to context and found 2 gaps this session.")
        );
        assert_eq!(
            sentence(&clauses(&[
                "added 3 of 12 ADRs to context",
                "found 2 gaps",
                "used 48k tokens over 14 turns"
            ]))
            .as_deref(),
            Some(
                "Actual AI: added 3 of 12 ADRs to context, found 2 gaps, and used 48k tokens over 14 turns this session."
            )
        );
    }

    #[test]
    fn test_adrs_is_singular_only_when_y_is_one() {
        let clause = |added, available| added_clause(Reported { added, available });
        assert_eq!(clause(1, 1), "added 1 of 1 ADR to context");
        assert_eq!(clause(3, 12), "added 3 of 12 ADRs to context");
        assert_eq!(clause(1, 2), "added 1 of 2 ADRs to context");
        assert_eq!(clause(0, 0), "added 0 of 0 ADRs to context");
    }

    // ── the hook ─────────────────────────────────────────────────────────

    /// The first response after a decision is briefed shows the count, and
    /// records it as shown; the next response, with nothing new, is silent.
    #[test]
    fn test_the_hook_reports_a_change_once() {
        with_scratch_config(|| {
            let root = repo();
            brief(root.path(), &["Adopt RS256"]);

            assert_eq!(
                hook_line(&stop("s1"), &hook_args(root.path())).as_deref(),
                Some("Actual AI: added 1 of 2 ADRs to context this session.")
            );
            assert_eq!(
                recorded(root.path()).last_reported,
                Some(Reported {
                    added: 1,
                    available: 2
                })
            );
            assert_eq!(hook_line(&stop("s1"), &hook_args(root.path())), None);
        });
    }

    /// A higher X shows the line again.
    #[test]
    fn test_the_hook_reports_again_when_more_decisions_are_added() {
        with_scratch_config(|| {
            let root = repo();
            brief(root.path(), &["Adopt RS256"]);
            assert!(hook_line(&stop("s1"), &hook_args(root.path())).is_some());

            brief(root.path(), &["provider-pinning"]);

            assert_eq!(
                hook_line(&stop("s1"), &hook_args(root.path())).as_deref(),
                Some("Actual AI: added 2 of 2 ADRs to context this session.")
            );
            assert_eq!(hook_line(&stop("s1"), &hook_args(root.path())), None);
        });
    }

    /// Y is read at every response, so a rule set that gains a decision shows
    /// the line again with X unchanged.
    #[test]
    fn test_the_hook_reports_again_when_the_rule_set_changes() {
        with_scratch_config(|| {
            let root = repo();
            brief(root.path(), &["Adopt RS256"]);
            assert!(hook_line(&stop("s1"), &hook_args(root.path())).is_some());

            std::fs::write(
                crate::rules::rules_dir(root.path()).join("caching-writes.md"),
                ADVICE.replace("SHOULD", "MUST").replace("Reads", "Writes"),
            )
            .unwrap();

            assert_eq!(
                hook_line(&stop("s1"), &hook_args(root.path())).as_deref(),
                Some("Actual AI: added 1 of 3 ADRs to context this session.")
            );
        });
    }

    /// No decision briefed means no line, and nothing written either.
    #[test]
    fn test_the_hook_is_silent_when_nothing_was_added() {
        with_scratch_config(|| {
            let root = repo();

            assert_eq!(hook_line(&stop("s1"), &hook_args(root.path())), None);
            assert_eq!(recorded(root.path()), BriefLedger::default());
        });
    }

    /// X counts only decisions the rule set still holds: one briefed and then
    /// removed from the rules is not "added X of Y" with X past Y, and a
    /// record of removed decisions alone is X of 0, which is silence.
    #[test]
    fn test_x_counts_only_decisions_the_rule_set_still_holds() {
        with_scratch_config(|| {
            let root = repo();
            brief(root.path(), &["Retired Decision"]);
            assert_eq!(hook_line(&stop("s1"), &hook_args(root.path())), None);

            brief(root.path(), &["Adopt RS256"]);
            assert_eq!(
                hook_line(&stop("s1"), &hook_args(root.path())).as_deref(),
                Some("Actual AI: added 1 of 2 ADRs to context this session.")
            );
        });
    }

    /// A repository with no rule set counts nothing, whatever the record says.
    #[test]
    fn test_the_hook_is_silent_without_rules() {
        with_scratch_config(|| {
            let root = tempdir().unwrap();
            brief(root.path(), &["Adopt RS256"]);

            assert_eq!(hook_line(&stop("s1"), &hook_args(root.path())), None);
        });
    }

    /// One decision in the whole rule set reads "1 of 1 ADR".
    #[test]
    fn test_the_hook_line_is_singular_for_a_single_adr() {
        with_scratch_config(|| {
            let root = repo_with(&[("provider-pinning.md", PINNING)]);
            brief(root.path(), &["provider-pinning"]);

            assert_eq!(
                hook_line(&stop("s1"), &hook_args(root.path())).as_deref(),
                Some("Actual AI: added 1 of 1 ADR to context this session.")
            );
        });
    }

    #[test]
    fn test_a_malformed_or_sessionless_envelope_is_silent() {
        with_scratch_config(|| {
            let root = repo();
            brief(root.path(), &["Adopt RS256"]);

            for raw in ["", "{ not json", "[1, 2]", "{}", r#"{"session_id": ""}"#] {
                assert_eq!(hook_line(raw, &hook_args(root.path())), None, "{raw}");
            }
            assert_eq!(recorded(root.path()).last_reported, None);
        });
    }

    /// A rule set that cannot be read is silence under the hook, not an error.
    #[test]
    fn test_an_unreadable_rule_set_is_silent_under_the_hook() {
        with_scratch_config(|| {
            let root = tempdir().unwrap();
            std::fs::create_dir_all(root.path().join(".actual")).unwrap();
            std::fs::write(crate::rules::rules_dir(root.path()), "not a directory").unwrap();
            brief(root.path(), &["Adopt RS256"]);

            assert_eq!(hook_line(&stop("s1"), &hook_args(root.path())), None);
        });
    }

    /// The summary keys on the repository it is given, never on the
    /// envelope's `cwd`, even when that names another governed repository.
    #[test]
    fn test_the_hook_reads_the_rule_set_it_is_given_not_the_envelopes_cwd() {
        with_scratch_config(|| {
            let root = repo();
            let elsewhere = repo_with(&[("provider-pinning.md", PINNING)]);
            brief(root.path(), &["Adopt RS256"]);
            let mut envelope: serde_json::Value = serde_json::from_str(&stop("s1")).unwrap();
            envelope["cwd"] = serde_json::json!(elsewhere.path().to_string_lossy());

            assert_eq!(
                hook_line(&envelope.to_string(), &hook_args(root.path())).as_deref(),
                Some("Actual AI: added 1 of 2 ADRs to context this session.")
            );
        });
    }

    /// `--rules-dir` spelled differently from the brief's still finds the
    /// record the brief wrote.
    #[test]
    fn test_the_hook_finds_the_record_under_another_spelling_of_the_rules_dir() {
        with_scratch_config(|| {
            let root = repo();
            brief(root.path(), &["Adopt RS256"]);
            let rules_dir = crate::rules::rules_dir(root.path());
            let mut args = hook_args(root.path());
            args.rules_dir = Some(rules_dir.join("..").join(rules_dir.file_name().unwrap()));

            assert!(hook_line(&stop("s1"), &args).is_some());
        });
    }

    /// A run that cannot take the record's lock prints nothing, because it
    /// could not record the count as shown.
    #[test]
    fn test_the_hook_is_silent_when_the_record_cannot_be_locked() {
        with_scratch_config(|| {
            let root = repo();
            brief(root.path(), &["Adopt RS256"]);
            // The lock file is `brief_ledger`'s `ledger.lock`; a directory in
            // its place cannot be opened, so no lock can be taken.
            let lock = brief_ledger::ledger_dir().unwrap().join("ledger.lock");
            std::fs::remove_file(&lock).unwrap();
            std::fs::create_dir(&lock).unwrap();

            assert_eq!(hook_line(&stop("s1"), &hook_args(root.path())), None);
            assert_eq!(recorded(root.path()).last_reported, None);
        });
    }

    /// Several runs reporting the same change at once print it once.
    #[test]
    fn test_concurrent_runs_report_one_change_once() {
        with_scratch_config(|| {
            let root = repo();
            brief(root.path(), &["Adopt RS256"]);
            // Warm the index cache, so the runs below only read it.
            rule_set_decisions(root.path(), &crate::rules::rules_dir(root.path())).unwrap();
            let runs = 8;
            let start = std::sync::Arc::new(std::sync::Barrier::new(runs));
            let handles: Vec<_> = (0..runs)
                .map(|_| {
                    let start = start.clone();
                    let args = hook_args(root.path());
                    std::thread::spawn(move || {
                        start.wait();
                        hook_line(&stop("s1"), &args)
                    })
                })
                .collect();
            let lines: Vec<String> = handles
                .into_iter()
                .filter_map(|handle| handle.join().unwrap())
                .collect();

            assert_eq!(
                lines,
                ["Actual AI: added 1 of 2 ADRs to context this session."]
            );
        });
    }

    /// Without a usable config directory there is no record to read.
    #[test]
    fn test_an_unusable_config_dir_is_silent_and_reads_as_nothing_added() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _dir = EnvGuard::set("ACTUAL_CONFIG_DIR", "not/absolute");
        let _file = EnvGuard::remove("ACTUAL_CONFIG");
        let root = repo();

        assert_eq!(hook_line(&stop("s1"), &hook_args(root.path())), None);
        assert_eq!(
            direct_output("s1", &direct_args(root.path()))
                .unwrap()
                .as_deref(),
            Some("Actual AI: added 0 of 2 ADRs to context this session.")
        );
    }

    // ── direct mode ──────────────────────────────────────────────────────

    /// Direct mode prints the sentence as it stands and leaves the record
    /// alone, so the hook still shows the change.
    #[test]
    fn test_direct_mode_prints_without_recording() {
        with_scratch_config(|| {
            let root = repo();
            brief(root.path(), &["Adopt RS256"]);

            assert_eq!(
                direct_output("s1", &direct_args(root.path()))
                    .unwrap()
                    .as_deref(),
                Some("Actual AI: added 1 of 2 ADRs to context this session.")
            );
            assert_eq!(recorded(root.path()).last_reported, None);
            assert!(hook_line(&stop("s1"), &hook_args(root.path())).is_some());
            // Already shown by the hook, and direct mode still prints it.
            assert!(direct_output("s1", &direct_args(root.path()))
                .unwrap()
                .is_some());
        });
    }

    /// Direct mode is the debugging view, so it prints even when X is 0.
    #[test]
    fn test_direct_mode_prints_when_nothing_was_added() {
        with_scratch_config(|| {
            let root = repo();
            assert_eq!(
                direct_output("s1", &direct_args(root.path()))
                    .unwrap()
                    .as_deref(),
                Some("Actual AI: added 0 of 2 ADRs to context this session.")
            );
        });
    }

    #[test]
    fn test_direct_mode_json_names_the_decisions_x_counted() {
        with_scratch_config(|| {
            let root = repo();
            brief(root.path(), &["Adopt RS256", "Retired Decision"]);
            let mut args = direct_args(root.path());
            args.json = true;

            let text = direct_output("s1", &args).unwrap().unwrap();
            let value: serde_json::Value = serde_json::from_str(&text).unwrap();

            assert_eq!(
                value,
                serde_json::json!({"x": 1, "y": 2, "decisions": ["Adopt RS256"]})
            );
        });
    }

    /// Outside the hook a rule set that cannot be read is an error, so a
    /// person debugging sees why there is no count.
    #[test]
    fn test_direct_mode_reports_an_unreadable_rule_set() {
        with_scratch_config(|| {
            let root = tempdir().unwrap();
            let mut args = direct_args(root.path());
            args.rules_dir = Some(root.path().join("rules-is-a-file"));
            std::fs::write(args.rules_dir.as_ref().unwrap(), "not a directory").unwrap();

            assert!(direct_output("s1", &args).is_err());
        });
    }

    // ── dispatch ─────────────────────────────────────────────────────────

    #[test]
    fn test_exec_runs_direct_mode() {
        with_scratch_config(|| {
            let root = repo();
            let args = SessionArgs {
                action: SessionAction::Summary(direct_args(root.path())),
            };
            assert!(exec(&args).is_ok());
        });
    }

    /// Under a test harness stdin carries no envelope, which is silence and
    /// exit 0. This reads the real stdin, so it holds no `ENV_MUTEX`: a stdin
    /// that never ends would otherwise stall every test that redirects the
    /// config directory.
    #[test]
    fn test_exec_hook_mode_exits_zero() {
        let root = repo();
        assert!(exec_summary(&hook_args(root.path())).is_ok());
    }

    #[test]
    fn test_exec_without_a_session_or_the_hook_is_an_error() {
        let root = tempdir().unwrap();
        let mut args = direct_args(root.path());
        args.session = None;
        assert!(matches!(
            exec_summary(&args),
            Err(ActualError::ConfigError(_))
        ));
    }

    #[test]
    fn test_emit_prints_a_line_or_nothing() {
        emit(Some(
            "Actual AI: added 1 of 1 ADR to context this session.".to_string(),
        ));
        emit(None);
    }
}
