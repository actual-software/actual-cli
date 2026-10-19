//! `actual session summary`: one line on what Actual AI did in a coding
//! session, for a Claude Code `Stop` hook.
//!
//! # Design
//!
//! **One line, built from clauses.** "3 of 12 ADRs added to context" and "2
//! areas with no ADR" are joined by " · ", and [`line`] is the one place that
//! builds the line, for plain output, `--json` and the hook alike. A session
//! that added no ADR and worked only where none applies gets one clause for
//! both, "no ADR covers the 3 areas you worked in". One that also worked in a
//! covered area keeps the count, "0 of 12 ADRs added to context · 2 areas with
//! no ADR", because its gaps are not every area it worked in. A rule set
//! holding no decision has no line.
//!
//! **X and Y.** X is the decisions [`super::brief_ledger`] recorded as added to
//! the context of this session's agents. Y is the decisions the rule set can
//! add to context at all, [`ScopeIndex::decision_keys`]. Both use the key
//! `rules brief` groups by, and X counts only recorded decisions that are still
//! in Y, so a rule set edited mid-session can lower X but never lift it past Y.
//!
//! **Gaps.** The folders the same record noted as worked in with no decision
//! governing any of their files. Their names stay on the machine: `--json`
//! lists them, and nothing sends them anywhere. Every other folder it noted
//! is covered, even when `--limit 0` or a budget too small for any rule
//! briefed nothing there.
//!
//! **Shown when it changes.** A plugin cannot speak once at the end of a
//! session, so the hook speaks at the end of a response, when a decision was
//! added or a gap found and the line differs from the one the user last saw.
//! The record's `last_reported` holds the counts behind that line, and it is
//! updated under the record's lock before the line is printed. A run that
//! cannot take the lock prints nothing, so two runs at once report a change
//! once.
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

/// `--json`: X, Y and the gaps, what they count, and the line itself.
#[derive(Serialize)]
struct SummaryJson<'a> {
    x: usize,
    y: usize,
    /// The decisions X counted.
    decisions: Vec<&'a str>,
    gaps: usize,
    /// The folders `gaps` counted, sorted.
    areas_without_adr: Vec<&'a str>,
    /// What plain output prints, or `null` when it prints nothing.
    line: Option<String>,
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
    let Some(session_id) = envelope.session_id.filter(|id| !id.is_empty()) else {
        // Not `?`: CodeQL reads that early return as the session id reaching `emit`.
        return None;
    };
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
    if recorded.briefed.is_empty() && recorded.gaps().next().is_none() {
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
    line(shown?)
}

/// Direct mode: the line for `session_id` as it stands, or its counts as
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
    let now = counts(&recorded, &decisions);

    if args.json {
        let json = serde_json::to_string_pretty(&SummaryJson {
            x: now.added,
            y: now.available,
            decisions: added(&recorded, &decisions).map(String::as_str).collect(),
            gaps: now.gaps,
            areas_without_adr: recorded.gaps().collect(),
            line: line(now),
        })
        .expect("session summary is serializable — this is a programmer error");
        return Ok(Some(json));
    }
    Ok(line(now))
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

/// X, Y, the gaps and the covered folders for one record against one rule
/// set.
fn counts(recorded: &BriefLedger, decisions: &BTreeSet<String>) -> Reported {
    Reported {
        added: added(recorded, decisions).count(),
        available: decisions.len(),
        gaps: recorded.gaps().count(),
        covered: recorded.covered_folders.len(),
    }
}

/// The counts to show, when they are worth showing: a decision was added or a
/// gap found, and the line differs from the one built from the counts last
/// shown.
fn due(recorded: &BriefLedger, decisions: &BTreeSet<String>) -> Option<Reported> {
    let now = counts(recorded, decisions);
    if now.added == 0 && now.gaps == 0 {
        return None;
    }
    let text = line(now)?;
    (recorded.last_reported.and_then(line) != Some(text)).then_some(now)
}

/// The summary line for `counts`, or `None` when the rule set holds no
/// decision.
fn line(counts: Reported) -> Option<String> {
    if counts.available == 0 {
        return None;
    }
    let clauses = match (counts.added, counts.gaps, counts.covered) {
        (0, gaps, 0) if gaps > 0 => vec![uncovered_clause(gaps)],
        (_, 0, _) => vec![added_clause(counts)],
        (_, gaps, _) => vec![added_clause(counts), gaps_clause(gaps)],
    };
    Some(format!("Actual AI: {}", clauses.join(" · ")))
}

/// "3 of 12 ADRs added to context", with "ADR" singular when Y is 1.
fn added_clause(counts: Reported) -> String {
    let noun = if counts.available == 1 { "ADR" } else { "ADRs" };
    format!(
        "{} of {} {noun} added to context",
        counts.added, counts.available
    )
}

/// "2 areas with no ADR", or "1 area with no ADR".
fn gaps_clause(gaps: usize) -> String {
    let noun = if gaps == 1 { "area" } else { "areas" };
    format!("{gaps} {noun} with no ADR")
}

/// "no ADR covers the 3 areas you worked in", or "the area" for one.
fn uncovered_clause(gaps: usize) -> String {
    if gaps == 1 {
        "no ADR covers the area you worked in".to_string()
    } else {
        format!("no ADR covers the {gaps} areas you worked in")
    }
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

    fn json_args(root: &Path) -> SessionSummaryArgs {
        SessionSummaryArgs {
            json: true,
            ..direct_args(root)
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

    /// Note `folder` in session `s1`, covered or a gap, as `rules brief` does.
    fn work_in(root: &Path, folder: &str, covered: bool) {
        let dir = brief_ledger::ledger_dir().unwrap();
        let rules_dir = key_dir(root);
        let key = LedgerKey {
            session_id: "s1",
            rules_dir: &rules_dir,
        };
        brief_ledger::record_folder(&dir, &key, folder, covered);
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

    fn json(root: &Path) -> serde_json::Value {
        let text = direct_output("s1", &json_args(root)).unwrap().unwrap();
        serde_json::from_str(&text).unwrap()
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

    // ── the line ─────────────────────────────────────────────────────────

    /// The line for a session that worked in no covered folder.
    fn shown(added: usize, available: usize, gaps: usize) -> Option<String> {
        line(Reported {
            added,
            available,
            gaps,
            covered: 0,
        })
    }

    /// The gaps stand alone only when they are every area the session worked
    /// in: nothing added and no folder covered.
    #[test]
    fn test_the_line_takes_every_form() {
        for (added, gaps, covered, expected) in [
            (
                3,
                2,
                0,
                "Actual AI: 3 of 12 ADRs added to context · 2 areas with no ADR",
            ),
            (
                3,
                1,
                2,
                "Actual AI: 3 of 12 ADRs added to context · 1 area with no ADR",
            ),
            (3, 0, 2, "Actual AI: 3 of 12 ADRs added to context"),
            (
                0,
                2,
                1,
                "Actual AI: 0 of 12 ADRs added to context · 2 areas with no ADR",
            ),
            (
                0,
                1,
                3,
                "Actual AI: 0 of 12 ADRs added to context · 1 area with no ADR",
            ),
            (
                0,
                3,
                0,
                "Actual AI: no ADR covers the 3 areas you worked in",
            ),
            (0, 1, 0, "Actual AI: no ADR covers the area you worked in"),
            (0, 0, 1, "Actual AI: 0 of 12 ADRs added to context"),
            (0, 0, 0, "Actual AI: 0 of 12 ADRs added to context"),
        ] {
            let counts = Reported {
                added,
                available: 12,
                gaps,
                covered,
            };
            assert_eq!(line(counts).as_deref(), Some(expected), "{counts:?}");
        }
    }

    #[test]
    fn test_adrs_is_singular_only_when_y_is_one() {
        assert_eq!(
            shown(1, 1, 0).as_deref(),
            Some("Actual AI: 1 of 1 ADR added to context")
        );
        assert_eq!(
            shown(0, 1, 0).as_deref(),
            Some("Actual AI: 0 of 1 ADR added to context")
        );
        assert_eq!(
            shown(1, 1, 2).as_deref(),
            Some("Actual AI: 1 of 1 ADR added to context · 2 areas with no ADR")
        );
        assert_eq!(
            shown(1, 2, 0).as_deref(),
            Some("Actual AI: 1 of 2 ADRs added to context")
        );
    }

    /// No rule set, or one holding no decision, has no line, whatever the
    /// record holds.
    #[test]
    fn test_there_is_no_line_without_a_decision_to_add() {
        for (added, gaps) in [(0, 0), (0, 2), (1, 0)] {
            assert_eq!(shown(added, 0, gaps), None, "{added} added, {gaps} gaps");
        }
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
                Some("Actual AI: 1 of 2 ADRs added to context")
            );
            assert_eq!(
                recorded(root.path()).last_reported,
                Some(Reported {
                    added: 1,
                    available: 2,
                    gaps: 0,
                    covered: 0
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
                Some("Actual AI: 2 of 2 ADRs added to context")
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
                Some("Actual AI: 1 of 3 ADRs added to context")
            );
        });
    }

    /// No decision briefed and no gap means no line, and nothing written
    /// either.
    #[test]
    fn test_the_hook_is_silent_when_nothing_was_added() {
        with_scratch_config(|| {
            let root = repo();

            assert_eq!(hook_line(&stop("s1"), &hook_args(root.path())), None);
            assert_eq!(recorded(root.path()), BriefLedger::default());

            work_in(root.path(), "services/auth", true);
            assert_eq!(hook_line(&stop("s1"), &hook_args(root.path())), None);
            assert_eq!(recorded(root.path()).last_reported, None);
        });
    }

    /// A session that added no decision and worked where none applies still
    /// hears about it, once per change to the line.
    #[test]
    fn test_the_hook_speaks_for_gaps_with_no_decision_added() {
        with_scratch_config(|| {
            let root = repo();
            work_in(root.path(), "infra", false);

            assert_eq!(
                hook_line(&stop("s1"), &hook_args(root.path())).as_deref(),
                Some("Actual AI: no ADR covers the area you worked in")
            );
            assert_eq!(
                recorded(root.path()).last_reported,
                Some(Reported {
                    added: 0,
                    available: 2,
                    gaps: 1,
                    covered: 0
                })
            );
            assert_eq!(hook_line(&stop("s1"), &hook_args(root.path())), None);

            work_in(root.path(), "scripts", false);
            assert_eq!(
                hook_line(&stop("s1"), &hook_args(root.path())).as_deref(),
                Some("Actual AI: no ADR covers the 2 areas you worked in")
            );
        });
    }

    /// The state a `--limit 0` brief leaves: `ops` covered with nothing added,
    /// and `infra/terraform` a gap. The gap is not every area the session
    /// worked in, so the line keeps the count.
    #[test]
    fn test_the_hook_keeps_the_count_when_a_covered_area_added_nothing() {
        with_scratch_config(|| {
            let root = repo();
            work_in(root.path(), "ops", true);
            work_in(root.path(), "infra/terraform", false);
            let line = "Actual AI: 0 of 2 ADRs added to context · 1 area with no ADR";

            assert_eq!(
                direct_output("s1", &direct_args(root.path()))
                    .unwrap()
                    .as_deref(),
                Some(line)
            );
            assert_eq!(
                hook_line(&stop("s1"), &hook_args(root.path())).as_deref(),
                Some(line)
            );
            assert_eq!(
                recorded(root.path()).last_reported,
                Some(Reported {
                    added: 0,
                    available: 2,
                    gaps: 1,
                    covered: 1
                })
            );
            assert_eq!(hook_line(&stop("s1"), &hook_args(root.path())), None);
        });
    }

    /// A session shown its gaps alone hears again once it works in a covered
    /// area, because the line now keeps the count. Another covered area leaves
    /// the line as it was.
    #[test]
    fn test_the_hook_speaks_again_when_a_covered_area_follows_the_gaps() {
        with_scratch_config(|| {
            let root = repo();
            work_in(root.path(), "infra/terraform", false);
            assert_eq!(
                hook_line(&stop("s1"), &hook_args(root.path())).as_deref(),
                Some("Actual AI: no ADR covers the area you worked in")
            );

            work_in(root.path(), "ops", true);
            assert_eq!(
                hook_line(&stop("s1"), &hook_args(root.path())).as_deref(),
                Some("Actual AI: 0 of 2 ADRs added to context · 1 area with no ADR")
            );

            work_in(root.path(), "services/auth", true);
            assert_eq!(hook_line(&stop("s1"), &hook_args(root.path())), None);
        });
    }

    /// Gaps join the count, and a gap the session later covers drops out of
    /// the line.
    #[test]
    fn test_the_hook_reports_gaps_beside_the_count() {
        with_scratch_config(|| {
            let root = repo();
            brief(root.path(), &["Adopt RS256"]);
            work_in(root.path(), "infra", false);

            assert_eq!(
                hook_line(&stop("s1"), &hook_args(root.path())).as_deref(),
                Some("Actual AI: 1 of 2 ADRs added to context · 1 area with no ADR")
            );

            work_in(root.path(), "infra", true);
            assert_eq!(
                hook_line(&stop("s1"), &hook_args(root.path())).as_deref(),
                Some("Actual AI: 1 of 2 ADRs added to context")
            );
        });
    }

    /// The hook compares lines, not counts: Y moving while the line names no
    /// Y prints nothing new.
    #[test]
    fn test_the_hook_is_silent_when_only_an_unshown_count_moves() {
        with_scratch_config(|| {
            let root = repo();
            work_in(root.path(), "infra", false);
            assert!(hook_line(&stop("s1"), &hook_args(root.path())).is_some());

            std::fs::write(
                crate::rules::rules_dir(root.path()).join("caching-writes.md"),
                ADVICE.replace("SHOULD", "MUST").replace("Reads", "Writes"),
            )
            .unwrap();

            assert_eq!(hook_line(&stop("s1"), &hook_args(root.path())), None);
        });
    }

    /// A record v0.7.0 wrote, whose `last_reported` has no gap count, keeps
    /// its decisions: the unchanged line is not shown again, and the first
    /// gap is, with the decision still counted.
    #[test]
    fn test_the_hook_reads_a_last_reported_with_no_gap_count() {
        with_scratch_config(|| {
            let root = repo();
            brief(root.path(), &["Adopt RS256"]);
            let records: Vec<PathBuf> = std::fs::read_dir(brief_ledger::ledger_dir().unwrap())
                .unwrap()
                .flatten()
                .map(|entry| entry.path())
                .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
                .collect();
            assert_eq!(records.len(), 1, "one record expected: {records:?}");
            let record = &records[0];
            std::fs::write(
                record,
                r#"{"format_version":1,"briefed":["Adopt RS256"],"last_reported":{"added":1,"available":2}}"#,
            )
            .unwrap();

            assert_eq!(hook_line(&stop("s1"), &hook_args(root.path())), None);

            work_in(root.path(), "infra", false);
            assert_eq!(
                hook_line(&stop("s1"), &hook_args(root.path())).as_deref(),
                Some("Actual AI: 1 of 2 ADRs added to context · 1 area with no ADR")
            );
            let after = recorded(root.path());
            assert!(after.briefed.contains("Adopt RS256"));
            assert_eq!(
                after.last_reported,
                Some(Reported {
                    added: 1,
                    available: 2,
                    gaps: 1,
                    covered: 0
                })
            );
        });
    }

    /// X counts only decisions the rule set still holds: one briefed and then
    /// removed from the rules is not "X of Y" with X past Y, and a record of
    /// removed decisions alone counts 0, which is silence.
    #[test]
    fn test_x_counts_only_decisions_the_rule_set_still_holds() {
        with_scratch_config(|| {
            let root = repo();
            brief(root.path(), &["Retired Decision"]);
            assert_eq!(hook_line(&stop("s1"), &hook_args(root.path())), None);

            brief(root.path(), &["Adopt RS256"]);
            assert_eq!(
                hook_line(&stop("s1"), &hook_args(root.path())).as_deref(),
                Some("Actual AI: 1 of 2 ADRs added to context")
            );
        });
    }

    /// A repository with no rule set counts nothing, whatever the record says.
    #[test]
    fn test_the_hook_is_silent_without_rules() {
        with_scratch_config(|| {
            let root = tempdir().unwrap();
            brief(root.path(), &["Adopt RS256"]);
            work_in(root.path(), "infra", false);

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
                Some("Actual AI: 1 of 1 ADR added to context")
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
                Some("Actual AI: 1 of 2 ADRs added to context")
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

            assert_eq!(lines, ["Actual AI: 1 of 2 ADRs added to context"]);
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
            Some("Actual AI: 0 of 2 ADRs added to context")
        );
    }

    // ── direct mode ──────────────────────────────────────────────────────

    /// Direct mode prints the line as it stands and leaves the record alone,
    /// so the hook still shows the change.
    #[test]
    fn test_direct_mode_prints_without_recording() {
        with_scratch_config(|| {
            let root = repo();
            brief(root.path(), &["Adopt RS256"]);

            assert_eq!(
                direct_output("s1", &direct_args(root.path()))
                    .unwrap()
                    .as_deref(),
                Some("Actual AI: 1 of 2 ADRs added to context")
            );
            assert_eq!(recorded(root.path()).last_reported, None);
            assert!(hook_line(&stop("s1"), &hook_args(root.path())).is_some());
            // Already shown by the hook, and direct mode still prints it.
            assert!(direct_output("s1", &direct_args(root.path()))
                .unwrap()
                .is_some());
        });
    }

    /// Direct mode is the debugging view, so it prints even when nothing was
    /// added and no gap found.
    #[test]
    fn test_direct_mode_prints_when_nothing_was_added() {
        with_scratch_config(|| {
            let root = repo();
            assert_eq!(
                direct_output("s1", &direct_args(root.path()))
                    .unwrap()
                    .as_deref(),
                Some("Actual AI: 0 of 2 ADRs added to context")
            );
        });
    }

    /// A rule set holding no decision prints nothing, and its JSON line is
    /// `null` with the counts still beside it.
    #[test]
    fn test_direct_mode_prints_nothing_for_a_rule_set_holding_no_decision() {
        with_scratch_config(|| {
            let root = repo_with(&[("caching-reads.md", ADVICE)]);
            work_in(root.path(), "infra", false);

            assert_eq!(
                direct_output("s1", &direct_args(root.path())).unwrap(),
                None
            );
            assert_eq!(
                json(root.path()),
                serde_json::json!({
                    "x": 0,
                    "y": 0,
                    "decisions": [],
                    "gaps": 1,
                    "areas_without_adr": ["infra"],
                    "line": null
                })
            );
        });
    }

    #[test]
    fn test_direct_mode_json_names_the_decisions_x_counted() {
        with_scratch_config(|| {
            let root = repo();
            brief(root.path(), &["Adopt RS256", "Retired Decision"]);

            assert_eq!(
                json(root.path()),
                serde_json::json!({
                    "x": 1,
                    "y": 2,
                    "decisions": ["Adopt RS256"],
                    "gaps": 0,
                    "areas_without_adr": [],
                    "line": "Actual AI: 1 of 2 ADRs added to context"
                })
            );
        });
    }

    /// The JSON lists the gaps sorted, leaves covered folders out, and its
    /// line is the one plain output prints, which keeps the count because
    /// two of the four areas are covered.
    #[test]
    fn test_direct_mode_json_lists_the_areas_with_no_adr() {
        with_scratch_config(|| {
            let root = repo();
            for (folder, covered) in [
                ("web/app", false),
                ("services/auth", true),
                ("infra", false),
                ("services/billing", false),
                ("services/billing", true),
            ] {
                work_in(root.path(), folder, covered);
            }
            let line = "Actual AI: 0 of 2 ADRs added to context · 2 areas with no ADR";

            assert_eq!(
                direct_output("s1", &direct_args(root.path()))
                    .unwrap()
                    .as_deref(),
                Some(line)
            );
            assert_eq!(
                json(root.path()),
                serde_json::json!({
                    "x": 0,
                    "y": 2,
                    "decisions": [],
                    "gaps": 2,
                    "areas_without_adr": ["infra", "web/app"],
                    "line": line
                })
            );
            assert_eq!(recorded(root.path()).last_reported, None);
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
        emit(Some("Actual AI: 1 of 1 ADR added to context".to_string()));
        emit(None);
    }
}
