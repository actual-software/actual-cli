//! `actual impl-check` — evaluate a `git diff` against the rule documents
//! that govern it.
//!
//! # Design
//!
//! `plan-check`'s implementation-stage counterpart (AK-755): the same
//! committed `.actual/rules/` corpus, the same shared pipeline
//! (`crate::cli::commands::check_engine::run_pipeline`), and the same
//! `--claude-hook` revision loop — see `plan_check`'s own module doc for the
//! full design rationale (the advisory-gate posture, the fail-open
//! conditions, the round-limit/override machinery), all of which applies
//! here unchanged. This module only differs in *what* it resolves to hand
//! the pipeline: a `git diff` instead of plan text.
//!
//! **Diff resolution has no envelope-carried source.** `plan-check
//! --claude-hook` can read `tool_input.plan` straight out of the
//! `PreToolUse` envelope, because Claude Code injects the plan there itself.
//! There is no equivalent for an implementation diff — no tool call carries
//! one — so `--claude-hook` mode here always shells out to `git diff HEAD` in
//! the resolved repository. Direct mode additionally accepts `--diff-file`
//! (for scripting/testing) and piped stdin, in that priority order, falling
//! back to `git diff HEAD` only when neither is given and stdin is not
//! piped — see [`resolve_direct_diff`].
//!
//! **An empty diff is not a user error.** `plan-check`'s direct mode refuses
//! an empty plan outright (`resolve_direct_plan` returns
//! `ActualError::ConfigError`): a plan is something a human or agent is
//! expected to have written, so nothing at all is almost always a mistake. A
//! diff is different — `git diff HEAD` legitimately returns nothing the
//! moment the working tree matches `HEAD`, which is an entirely ordinary
//! state (freshly cloned, freshly committed, nothing touched yet), not a
//! missing argument. So an empty diff from *any* source here (including an
//! explicitly empty `--diff-file` or empty piped stdin, for the same
//! uniform-treatment reason) is handled as "nothing to check": direct mode
//! prints a clean, informational result and exits 0 rather than erroring,
//! and `--claude-hook` mode emits a non-blocking notice and returns, mirroring
//! exactly how `plan_check_hook::resolve_plan` returning `None` is handled in
//! `plan_check::exec_hook_with`.
//!
//! **The revision loop needs no plan-check bootstrap.** `GovernanceSession`
//! (see `governance_session`) is keyed generically on `(session_id,
//! rules_dir)` with zero plan-specific coupling — `impl-check --claude-hook`
//! run with a `session_id` no `plan-check` session has ever touched simply
//! starts from `GovernanceSession::default()`, the same as any other new
//! session. There is nothing here to wire up for that to work.

use std::io::{IsTerminal, Read};
use std::path::Path;

use crate::cli::args::ImplCheckArgs;
use crate::cli::commands::check_engine::{
    self, capped_read, deny_summary, hook_deny_reason, override_reminder, partial_coverage_note,
    render_json, render_panel, round_limit_message, run_pipeline, with_override_reminder,
    Outcome,
};
use crate::cli::commands::governance_session::{self, GovernanceSession};
use crate::cli::commands::impl_check_hook::HookEnvelope;
use crate::cli::commands::plan_check::repo_root;
use crate::cli::commands::plan_check_hook;
use crate::cli::ui::term_size;
use crate::error::ActualError;
use crate::rules::check::{ArtifactKind, CheckedRule, Verdict};

#[cfg(feature = "telemetry")]
use crate::cli::commands::check_engine::{send_governance_events, send_hook_governance_events};

impl ImplCheckArgs {
    /// Project this command's own scalar fields into the shape
    /// [`run_pipeline`] actually consumes — see
    /// `check_engine::CheckKnobs`'s own doc for why this exists instead of
    /// `run_pipeline` depending on `PlanCheckArgs` directly.
    fn check_knobs(&self) -> check_engine::CheckKnobs<'_> {
        check_engine::CheckKnobs {
            rebuild: self.rebuild,
            limit: self.limit,
            candidates: self.candidates,
            runner: self.runner.as_ref(),
            model: self.model.as_deref(),
        }
    }
}

pub fn exec(args: &ImplCheckArgs) -> Result<(), ActualError> {
    if args.claude_hook {
        exec_hook(args);
        return Ok(());
    }
    exec_direct(args)
}

// ── direct mode ──────────────────────────────────────────────────────────

fn exec_direct(args: &ImplCheckArgs) -> Result<(), ActualError> {
    let root = repo_root(args.repo.as_ref());
    let Some(diff_text) = resolve_direct_diff(args, &root)? else {
        print_nothing_to_check(args.json);
        return Ok(());
    };
    let rules_dir = args
        .rules_dir
        .clone()
        .unwrap_or_else(|| crate::rules::rules_dir(&root));

    #[cfg(feature = "telemetry")]
    let started_at = std::time::Instant::now();

    let outcome = run_pipeline(
        &diff_text,
        &root,
        &rules_dir,
        &args.check_knobs(),
        ArtifactKind::Diff,
        !args.no_rank,
        &GovernanceSession::default(),
    )?;

    let width = term_size::terminal_width();
    if args.json {
        println!("{}", render_json(&outcome));
    } else {
        println!("{}", render_panel(&outcome, &diff_text, &rules_dir, width));
    }

    let result = if let Outcome::Verdicts { verdicts, .. } = &outcome {
        let conflicts: Vec<&CheckedRule> = verdicts.iter().filter(|v| v.verdict.blocks()).collect();
        if !conflicts.is_empty() {
            Err(ActualError::ImplNotConforming(deny_summary(&conflicts)))
        } else {
            Ok(())
        }
    } else {
        Ok(())
    };

    // Same reasoning as `plan_check::exec_direct`'s identical block: only
    // `Outcome::Verdicts` is a real governance decision, and
    // `RequiresDecision` maps to `warn` here via `verdict.blocks()`.
    #[cfg(feature = "telemetry")]
    if let Outcome::Verdicts {
        verdicts, partial, ..
    } = &outcome
    {
        let decision = if verdicts.iter().any(|v| v.verdict.blocks()) {
            crate::api::types::PlanGovernanceDecision::Block
        } else if verdicts
            .iter()
            .any(|v| v.verdict == Verdict::RequiresDecision)
            || partial.is_some()
        {
            crate::api::types::PlanGovernanceDecision::Warn
        } else {
            crate::api::types::PlanGovernanceDecision::Allow
        };
        let exit_code = result.as_ref().err().map(|e| e.exit_code()).unwrap_or(0);
        let violations: Vec<(&str, &str, crate::api::types::PlanGovernanceDecision)> = verdicts
            .iter()
            .filter(|v| v.verdict != Verdict::Conforming)
            .map(|v| {
                (
                    v.rule_id.as_str(),
                    v.doc_slug.as_str(),
                    crate::telemetry::plan_governance::rule_decision(v.verdict, v.verdict.blocks()),
                )
            })
            .collect();
        send_governance_events(
            "impl-check",
            &root,
            started_at,
            decision,
            exit_code,
            &violations,
        );
    }

    result
}

/// Print the "nothing to check" result for an empty diff: not an error (see
/// the module doc), just a clean, exit-0 report distinct from
/// `Outcome::NothingApplies` (which means "rules exist, but none apply to
/// this material" — a different fact than "there is no material at all").
fn print_nothing_to_check(json: bool) {
    if json {
        println!(
            "{}",
            serde_json::json!({
                "status": "not_checked",
                "detail": "the diff is empty -- nothing to check",
                "documents_selected": 0,
            })
        );
        return;
    }
    let width = term_size::terminal_width();
    let panel = crate::cli::ui::panel::Panel::titled("Implementation check")
        .separator()
        .line("Nothing to check: the diff is empty.")
        .render(width);
    println!("{panel}");
}

/// The diff text for direct-mode use: `--diff-file`, then piped stdin (when
/// stdin is not a real terminal — mirrors `check_engine::exec_override`'s own
/// `IsTerminal` check, so running this interactively with no piped input
/// falls through to `git diff HEAD` instead of blocking on a read from a
/// terminal that will never supply one), then `git diff HEAD` in the
/// resolved repo root.
///
/// Returns `None` when the resolved diff is empty — see the module doc for
/// why that is "nothing to check," not an error, unlike
/// `plan_check::resolve_direct_plan`'s treatment of an empty plan.
fn resolve_direct_diff(args: &ImplCheckArgs, root: &Path) -> Result<Option<String>, ActualError> {
    if let Some(path) = &args.diff_file {
        let file = std::fs::File::open(path).map_err(ActualError::IoError)?;
        let text = capped_read(file, &path.display().to_string())?;
        return Ok(non_empty(text));
    }
    if !std::io::stdin().is_terminal() {
        let text = capped_read(std::io::stdin(), "stdin")?;
        return Ok(non_empty(text));
    }
    Ok(non_empty(git_diff_head(root)?))
}

fn non_empty(text: String) -> Option<String> {
    if text.trim().is_empty() {
        None
    } else {
        Some(text)
    }
}

/// Shell out to `git diff HEAD` in `root`, capped at the same size limit
/// every other diff source is capped at ([`plan_check_hook::MAX_READ_BYTES`],
/// via [`capped_read`], reused directly on the child's own piped stdout
/// rather than buffering the whole subprocess output first and checking its
/// size only after).
///
/// Synchronous, deliberately: this is a local git operation with no network
/// involved, unlike this repo's async git-remote calls in `advisor.rs` /
/// `sync/cache.rs`, which specifically guard against a *remote* hang — there
/// is nothing analogous to wait out here, so no timeout/async machinery is
/// needed.
fn git_diff_head(root: &Path) -> Result<String, ActualError> {
    let mut child = std::process::Command::new("git")
        .args(["diff", "HEAD"])
        .current_dir(root)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(ActualError::IoError)?;
    let stdout = child.stdout.take().expect("stdout was piped");
    let text = capped_read(stdout, "git diff HEAD")?;
    let mut stderr = String::new();
    if let Some(mut err) = child.stderr.take() {
        let _ = err.read_to_string(&mut stderr);
    }
    let status = child.wait().map_err(ActualError::IoError)?;
    if !status.success() {
        return Err(ActualError::ConfigError(format!(
            "git diff HEAD failed in {}: {}",
            root.display(),
            stderr.trim()
        )));
    }
    Ok(text)
}

// ── --claude-hook mode ───────────────────────────────────────────────────

/// Read the hook payload from real stdin and hand it to [`exec_hook_with`].
///
/// See `plan_check::exec_hook`'s own doc comment for why this is kept to one
/// fallible line: the same real-stdin-in-a-test-harness hazard applies here.
fn exec_hook(args: &ImplCheckArgs) {
    let raw = match capped_read(std::io::stdin(), "the hook payload") {
        Ok(text) => text,
        Err(_) => {
            emit(plan_check_hook::render_notice(
                "impl-check could not read the hook payload on stdin",
            ));
            return;
        }
    };
    exec_hook_with(args, &raw);
}

/// Run the `--claude-hook` path against an already-read payload.
///
/// INVARIANT: same as `plan_check::exec_hook_with` — every fallible step
/// below is matched explicitly and turned into a fail-open notice (or, for a
/// real violation, a deny) rather than propagating an error, so this
/// function never reaches an ordinary nonzero exit on its own.
fn exec_hook_with(args: &ImplCheckArgs, raw: &str) {
    let envelope: HookEnvelope = match serde_json::from_str(raw) {
        Ok(envelope) => envelope,
        Err(_) => {
            emit(plan_check_hook::render_notice(
                "impl-check could not parse the hook payload as JSON",
            ));
            return;
        }
    };

    let root = repo_root(args.repo.as_ref());
    let rules_dir = args
        .rules_dir
        .clone()
        .unwrap_or_else(|| crate::rules::rules_dir(&root));

    // Unlike plan text, the diff is never carried on the envelope -- always
    // `git diff HEAD` in the resolved repo. See the module doc.
    let diff_text = match git_diff_head(&root) {
        Ok(text) => text,
        Err(e) => {
            emit(plan_check_hook::render_notice(&format!(
                "impl-check could not read git diff HEAD in {}: {e}",
                root.display()
            )));
            return;
        }
    };
    if diff_text.trim().is_empty() {
        emit(plan_check_hook::render_notice(
            "impl-check found no diff to check (git diff HEAD is empty).",
        ));
        return;
    }

    // The revision loop keys entirely on `session_id`, generically on
    // `(session_id, rules_dir)` — a session no prior `impl-check` (or
    // `plan-check`) call has ever touched simply starts fresh. See the
    // module doc.
    let session_id = envelope.session_id.as_deref();
    let mut session = session_id
        .map(|id| governance_session::load(id, &rules_dir))
        .unwrap_or_default();
    let diff_digest = governance_session::content_digest(&diff_text);

    #[cfg(feature = "telemetry")]
    let started_at = std::time::Instant::now();

    // `use_rank: false`, unconditionally, for the same reason
    // `plan_check::exec_hook_with` always passes it: the hook's one model
    // call stays reserved for the judge.
    let outcome = match run_pipeline(
        &diff_text,
        &root,
        &rules_dir,
        &args.check_knobs(),
        ArtifactKind::Diff,
        false,
        &session,
    ) {
        Ok(outcome) => outcome,
        Err(e) => {
            emit(plan_check_hook::render_notice(&format!(
                "impl-check could not read {}: {e}",
                rules_dir.display()
            )));
            return;
        }
    };

    match outcome {
        Outcome::NothingApplies => {
            emit(plan_check_hook::render_notice(
                "No committed rule under .actual/rules/ applies to this diff.",
            ));
        }
        Outcome::NoRunner { reason, .. } => {
            emit(plan_check_hook::render_notice(&format!(
                "Actual implementation governance did not run: no runner available ({reason})."
            )));
        }
        Outcome::CheckFailed { reason, .. } => {
            emit(plan_check_hook::render_notice(&format!(
                "Actual implementation governance did not run: {reason}"
            )));
        }
        Outcome::Verdicts {
            verdicts,
            runner_label,
            partial,
            ..
        } => {
            let judge_ran = runner_label.is_some();
            if session_id.is_some() {
                for v in &verdicts {
                    let key = governance_session::key(&v.doc_slug, &v.rule_id);
                    if v.verdict == Verdict::Conforming {
                        session.cleared.insert(key, diff_digest.clone());
                    } else {
                        session.cleared.remove(&key);
                    }
                }
                if judge_ran {
                    session.rounds += 1;
                }
            }

            let blocking: Vec<&CheckedRule> = verdicts
                .iter()
                .filter(|v| matches!(v.verdict, Verdict::Conflicting | Verdict::RequiresDecision))
                .collect();

            if !blocking.is_empty() {
                if session_id.is_some() {
                    for c in &blocking {
                        session.record_denial(&governance_session::key(&c.doc_slug, &c.rule_id));
                    }
                }

                if let Some(session_id) = session_id {
                    let exhausted: Vec<&CheckedRule> = blocking
                        .iter()
                        .filter(|c| {
                            session.deny_limit_exceeded(
                                &governance_session::key(&c.doc_slug, &c.rule_id),
                                args.max_rounds,
                            )
                        })
                        .copied()
                        .collect();
                    if !exhausted.is_empty() && exhausted.len() == blocking.len() {
                        let keys: Vec<String> = exhausted
                            .iter()
                            .map(|c| governance_session::key(&c.doc_slug, &c.rule_id))
                            .collect();
                        let message = round_limit_message(&exhausted, &session, args.max_rounds);
                        governance_session::record_round_limit(
                            session_id,
                            &rules_dir,
                            session.rounds,
                            &keys,
                            &message,
                        );
                        governance_session::store(session_id, &rules_dir, &session);
                        emit(plan_check_hook::render_notice(&with_override_reminder(
                            message, &session,
                        )));
                        #[cfg(feature = "telemetry")]
                        send_hook_governance_events(
                            "impl-check --claude-hook",
                            &root,
                            started_at,
                            crate::api::types::PlanGovernanceDecision::Warn,
                            &verdicts,
                            false,
                        );
                        return;
                    }
                    governance_session::store(session_id, &rules_dir, &session);
                }
                let deny_reason = hook_deny_reason(&blocking, session_id, partial);
                emit(plan_check_hook::render_deny(&with_override_reminder(
                    deny_reason,
                    &session,
                )));
                #[cfg(feature = "telemetry")]
                send_hook_governance_events(
                    "impl-check --claude-hook",
                    &root,
                    started_at,
                    crate::api::types::PlanGovernanceDecision::Block,
                    &verdicts,
                    true,
                );
                return;
            }

            if let Some(session_id) = session_id {
                governance_session::store(session_id, &rules_dir, &session);
            }

            let mut notes = Vec::new();
            if let Some((judged, total)) = partial {
                notes.push(partial_coverage_note(judged, total));
                if let Some(session_id) = session_id {
                    governance_session::record_partial_coverage(
                        session_id,
                        &rules_dir,
                        session.rounds,
                        judged,
                        total,
                    );
                    notes.push(
                        "This is not a silent pass — recorded in plan-check-overrides.log."
                            .to_string(),
                    );
                }
            }
            let reminder = override_reminder(&session);
            #[cfg(feature = "telemetry")]
            let has_override_reminder = reminder.is_some();
            if let Some(reminder) = reminder {
                notes.push(reminder);
            }
            if !notes.is_empty() {
                emit(plan_check_hook::render_notice(&notes.join("\n")));
            }
            #[cfg(feature = "telemetry")]
            {
                let decision = if partial.is_some() || has_override_reminder {
                    crate::api::types::PlanGovernanceDecision::Warn
                } else {
                    crate::api::types::PlanGovernanceDecision::Allow
                };
                send_hook_governance_events(
                    "impl-check --claude-hook",
                    &root,
                    started_at,
                    decision,
                    &verdicts,
                    false,
                );
            }
        }
    }
}

/// Print exactly one line — the same "stdout is exactly one JSON object, or
/// nothing" invariant `plan_check::emit` enforces.
fn emit(json: String) {
    println!("{json}");
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::cli::args::DEFAULT_MAX_ROUNDS;
    use std::path::{Path as StdPath, PathBuf};
    use tempfile::{tempdir, TempDir};

    use crate::testutil::{EnvGuard, ENV_MUTEX};

    #[cfg(feature = "telemetry")]
    use crate::cli::commands::check_engine::tests::with_captured_plan_governance_events;

    const OAUTH_DOC: &str = "# Sign With Asymmetric Keys: Token Signing\n\nThese rules are ALWAYS ACTIVE for OAuth token signing in `services/auth/oauth/`.\n\n### Rules\n\n- **R-A-001** MUST: sign with RS256.\n- **R-A-002** MUST NOT: log the raw signing key.\n";

    fn seed(files: &[(&str, &str)]) -> TempDir {
        let root = tempdir().unwrap();
        let dir = crate::rules::rules_dir(root.path());
        std::fs::create_dir_all(&dir).unwrap();
        for (name, contents) in files {
            std::fs::write(dir.join(name), contents).unwrap();
        }
        root
    }

    fn base_args() -> ImplCheckArgs {
        ImplCheckArgs {
            diff_file: None,
            repo: None,
            rules_dir: None,
            claude_hook: false,
            limit: 20,
            candidates: crate::rules::scope::DEFAULT_CANDIDATES,
            no_rank: false,
            runner: None,
            model: None,
            json: false,
            rebuild: false,
            max_rounds: DEFAULT_MAX_ROUNDS,
        }
    }

    fn isolated_config(home: &TempDir) -> (EnvGuard, EnvGuard) {
        (
            EnvGuard::set("ACTUAL_CONFIG_DIR", home.path().to_str().unwrap()),
            EnvGuard::remove("ACTUAL_CONFIG"),
        )
    }

    /// Run a git command in `cwd`, asserting it succeeds — mirrors
    /// `advisor.rs`'s own `run_git` test helper (private to that module, so
    /// not reusable directly) for building throwaway test repos.
    fn run_git(cwd: &StdPath, git_args: &[&str]) {
        let status = std::process::Command::new("git")
            .args(git_args)
            .current_dir(cwd)
            .env("GIT_AUTHOR_NAME", "Test")
            .env("GIT_AUTHOR_EMAIL", "test@example.com")
            .env("GIT_COMMITTER_NAME", "Test")
            .env("GIT_COMMITTER_EMAIL", "test@example.com")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .expect("git is available in the test environment");
        assert!(status.success(), "git {git_args:?} failed");
    }

    /// A real throwaway git repo with a committed baseline, so `git diff
    /// HEAD` has something to diff against. `oauth.rs` is committed at
    /// baseline (not left untracked) specifically so a test can overwrite
    /// its content afterward and have `git diff HEAD` actually show it —
    /// `git diff HEAD` only ever shows tracked-file changes, never an
    /// untracked new file, so a test that wants a real diff must modify a
    /// file this fixture already committed rather than writing a brand-new
    /// one.
    fn git_repo_with_baseline() -> TempDir {
        let dir = tempdir().unwrap();
        run_git(dir.path(), &["init", "-q"]);
        std::fs::write(dir.path().join("service.rs"), "fn handler() {}\n").unwrap();
        std::fs::write(dir.path().join("oauth.rs"), "fn sign() {}\n").unwrap();
        run_git(dir.path(), &["add", "."]);
        run_git(dir.path(), &["commit", "-q", "-m", "baseline"]);
        dir
    }

    #[cfg(unix)]
    fn fake_claude(dir: &StdPath, structured_output: &serde_json::Value) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;

        let envelope = serde_json::json!({
            "type": "result",
            "subtype": "success",
            "is_error": false,
            "structured_output": structured_output,
        })
        .to_string();
        let script = dir.join("fake-claude.sh");
        let body = format!(
            "#!/bin/sh\nif [ \"$1\" = \"auth\" ]; then printf '%s' '{{\"loggedIn\":true}}'; exit 0; fi\nprintf '%s' '{envelope}'\n"
        );
        std::fs::write(&script, body).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        script
    }

    fn check_output(entries: serde_json::Value) -> serde_json::Value {
        serde_json::json!({ "verdicts": entries })
    }

    // ── resolve_direct_diff / git_diff_head ─────────────────────────────

    #[test]
    fn test_resolve_direct_diff_reads_diff_file() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("the.diff");
        std::fs::write(&file, "diff --git a/x b/x\n+hello\n").unwrap();
        let mut args = base_args();
        args.diff_file = Some(file);
        let repo = tempdir().unwrap();
        let diff = resolve_direct_diff(&args, repo.path()).unwrap();
        assert_eq!(diff.as_deref(), Some("diff --git a/x b/x\n+hello\n"));
    }

    #[test]
    fn test_resolve_direct_diff_file_empty_is_nothing_to_check() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("empty.diff");
        std::fs::write(&file, "   \n\n").unwrap();
        let mut args = base_args();
        args.diff_file = Some(file);
        let repo = tempdir().unwrap();
        assert_eq!(resolve_direct_diff(&args, repo.path()).unwrap(), None);
    }

    #[test]
    fn test_resolve_direct_diff_errors_when_the_diff_file_does_not_exist() {
        let mut args = base_args();
        args.diff_file = Some(PathBuf::from("/no/such/diff-file.diff"));
        let repo = tempdir().unwrap();
        let err = resolve_direct_diff(&args, repo.path()).unwrap_err();
        assert!(matches!(err, ActualError::IoError(_)));
    }

    #[test]
    fn test_resolve_direct_diff_errors_when_the_diff_file_exceeds_the_size_limit() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("huge.diff");
        let oversized = vec![b'x'; (plan_check_hook::MAX_READ_BYTES + 1) as usize];
        std::fs::write(&file, &oversized).unwrap();
        let mut args = base_args();
        args.diff_file = Some(file);
        let repo = tempdir().unwrap();
        let err = resolve_direct_diff(&args, repo.path()).unwrap_err();
        assert!(matches!(err, ActualError::ConfigError(_)));
        assert!(err.to_string().contains("exceeds"));
    }

    #[test]
    fn test_git_diff_head_reads_a_real_working_tree_change() {
        let repo = git_repo_with_baseline();
        std::fs::write(repo.path().join("service.rs"), "fn handler() { todo!() }\n").unwrap();
        let diff = git_diff_head(repo.path()).unwrap();
        assert!(diff.contains("service.rs"));
        assert!(diff.contains("todo!()"));
    }

    #[test]
    fn test_git_diff_head_empty_when_nothing_changed() {
        let repo = git_repo_with_baseline();
        let diff = git_diff_head(repo.path()).unwrap();
        assert_eq!(diff.trim(), "");
    }

    #[test]
    fn test_git_diff_head_errors_outside_a_git_repository() {
        let not_a_repo = tempdir().unwrap();
        let err = git_diff_head(not_a_repo.path()).unwrap_err();
        assert!(matches!(err, ActualError::ConfigError(_)));
    }

    #[test]
    fn test_resolve_direct_diff_falls_back_to_git_diff_head_with_no_file_and_a_terminal_stdin() {
        // `IsTerminal` cannot be faked from an in-process test the same way
        // `check_engine::exec_override`'s own doc explains -- this exercises
        // the fallback shape indirectly, via `git_diff_head` itself, which is
        // the only real logic in that final branch. The subprocess-driven
        // integration test (`tests/cli_test.rs`) covers the full three-way
        // resolution order with piped stdin, which *is* controllable there.
        let repo = git_repo_with_baseline();
        std::fs::write(repo.path().join("service.rs"), "fn handler() { 1 }\n").unwrap();
        let diff = git_diff_head(repo.path()).unwrap();
        assert!(!diff.trim().is_empty());
    }

    // ── exec / exec_direct: dispatch and the conforming/conflict outcomes ───

    #[test]
    fn test_exec_direct_prints_nothing_to_check_on_an_empty_diff() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let repo = git_repo_with_baseline();
        let mut args = base_args();
        args.repo = Some(repo.path().to_path_buf());
        assert!(exec(&args).is_ok());
    }

    #[test]
    fn test_exec_direct_dispatch_with_no_applicable_rules_returns_ok() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let repo = git_repo_with_baseline();
        std::fs::write(repo.path().join("service.rs"), "fn handler() { 2 }\n").unwrap();
        let mut args = base_args();
        args.repo = Some(repo.path().to_path_buf());
        assert!(exec(&args).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn test_exec_direct_returns_ok_on_a_conforming_diff() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let root = seed(&[("cross-cutting-token-signing-1c57.md", OAUTH_DOC)]);
        let bin = tempdir().unwrap();
        let response = check_output(serde_json::json!([
            {"doc_slug": "cross-cutting-token-signing-1c57", "rule_id": "R-A-001", "verdict": "conforming", "span": "", "reason": "uses RS256"},
            {"doc_slug": "cross-cutting-token-signing-1c57", "rule_id": "R-A-002", "verdict": "conforming", "span": "", "reason": "no logging"},
        ]));
        let _binary = EnvGuard::set(
            "CLAUDE_BINARY",
            fake_claude(bin.path(), &response).to_str().unwrap(),
        );

        let mut args = base_args();
        args.diff_file = None;
        args.repo = Some(root.path().to_path_buf());
        args.runner = Some(crate::cli::args::RunnerChoice::ClaudeCli);
        args.no_rank = true;

        let diff_dir = tempdir().unwrap();
        let diff_file = diff_dir.path().join("the.diff");
        std::fs::write(
            &diff_file,
            "diff --git a/oauth.rs b/oauth.rs\n+sign access tokens with RS256\n",
        )
        .unwrap();
        args.diff_file = Some(diff_file);

        #[cfg(feature = "telemetry")]
        let events = with_captured_plan_governance_events(|| assert!(exec(&args).is_ok()));
        #[cfg(not(feature = "telemetry"))]
        assert!(exec(&args).is_ok());

        #[cfg(feature = "telemetry")]
        {
            let completed = events
                .iter()
                .find(|e| {
                    e.event
                        == crate::api::types::PlanGovernanceEventName::PlanGovernanceCheckCompleted
                })
                .expect("a completed event must be emitted");
            let props = completed.properties.as_ref().unwrap();
            assert_eq!(
                props.decision,
                Some(crate::api::types::PlanGovernanceDecision::Allow)
            );
            assert_eq!(props.command.as_deref(), Some("impl-check"));
        }
    }

    #[cfg(unix)]
    #[test]
    fn test_exec_direct_returns_impl_not_conforming_on_a_real_conflict() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let root = seed(&[("cross-cutting-token-signing-1c57.md", OAUTH_DOC)]);
        let bin = tempdir().unwrap();
        let response = check_output(serde_json::json!([
            {"doc_slug": "cross-cutting-token-signing-1c57", "rule_id": "R-A-001", "verdict": "conforming", "span": "", "reason": "uses RS256"},
            {"doc_slug": "cross-cutting-token-signing-1c57", "rule_id": "R-A-002", "verdict": "conflicting", "span": "logs the key", "reason": "forbidden"},
        ]));
        let _binary = EnvGuard::set(
            "CLAUDE_BINARY",
            fake_claude(bin.path(), &response).to_str().unwrap(),
        );

        let diff_dir = tempdir().unwrap();
        let diff_file = diff_dir.path().join("the.diff");
        std::fs::write(
            &diff_file,
            "diff --git a/oauth.rs b/oauth.rs\n+sign access tokens with RS256\n",
        )
        .unwrap();

        let mut args = base_args();
        args.diff_file = Some(diff_file);
        args.repo = Some(root.path().to_path_buf());
        args.runner = Some(crate::cli::args::RunnerChoice::ClaudeCli);
        args.no_rank = true;

        let err = exec(&args).unwrap_err();
        assert!(matches!(err, ActualError::ImplNotConforming(_)));
    }

    #[test]
    fn test_exec_direct_errors_when_the_rules_directory_cannot_be_read() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let root = tempdir().unwrap();
        let not_a_dir = root.path().join("rules-dir-is-a-file");
        std::fs::write(&not_a_dir, "not a directory").unwrap();

        let diff_dir = tempdir().unwrap();
        let diff_file = diff_dir.path().join("the.diff");
        std::fs::write(&diff_file, "+something").unwrap();

        let mut args = base_args();
        args.diff_file = Some(diff_file);
        args.rules_dir = Some(not_a_dir);
        assert!(exec(&args).is_err());
    }

    // ── exec_hook_with: notice/deny branches ────────────────────────────

    #[test]
    fn test_exec_hook_with_malformed_json_is_a_silent_fail_open() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        exec_hook_with(&base_args(), "not json");
    }

    #[test]
    fn test_exec_hook_with_reports_a_git_diff_failure_outside_a_repo() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let not_a_repo = tempdir().unwrap();
        let mut args = base_args();
        args.repo = Some(not_a_repo.path().to_path_buf());
        exec_hook_with(&args, "{}");
    }

    #[test]
    fn test_exec_hook_with_empty_diff_is_a_silent_fail_open() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let repo = git_repo_with_baseline();
        let mut args = base_args();
        args.repo = Some(repo.path().to_path_buf());
        exec_hook_with(&args, "{}");
    }

    #[test]
    fn test_exec_hook_with_reports_a_rules_directory_load_failure() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let repo = git_repo_with_baseline();
        std::fs::write(repo.path().join("service.rs"), "fn handler() { 3 }\n").unwrap();
        let not_a_dir = repo.path().join("rules-dir-is-a-file");
        std::fs::write(&not_a_dir, "not a directory").unwrap();

        let mut args = base_args();
        args.repo = Some(repo.path().to_path_buf());
        args.rules_dir = Some(not_a_dir);
        exec_hook_with(&args, "{}");
    }

    #[test]
    fn test_exec_hook_with_nothing_applies() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let repo = git_repo_with_baseline();
        std::fs::write(repo.path().join("service.rs"), "fn handler() { 4 }\n").unwrap();
        let mut args = base_args();
        args.repo = Some(repo.path().to_path_buf());
        exec_hook_with(&args, "{}");
    }

    #[cfg(unix)]
    #[test]
    fn test_exec_hook_with_denies_a_real_conflict() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let repo = git_repo_with_baseline();
        let rules = repo.path().join(".actual/rules");
        std::fs::create_dir_all(&rules).unwrap();
        std::fs::write(rules.join("cross-cutting-token-signing-1c57.md"), OAUTH_DOC).unwrap();
        std::fs::write(
            repo.path().join("oauth.rs"),
            "fn sign() { sign_with_rs256(); log_signing_key(); }\n",
        )
        .unwrap();

        let bin = tempdir().unwrap();
        let response = check_output(serde_json::json!([
            {"doc_slug": "cross-cutting-token-signing-1c57", "rule_id": "R-A-001", "verdict": "conforming", "span": "", "reason": "uses RS256"},
            {"doc_slug": "cross-cutting-token-signing-1c57", "rule_id": "R-A-002", "verdict": "conflicting", "span": "logs the key", "reason": "forbidden"},
        ]));
        let _binary = EnvGuard::set(
            "CLAUDE_BINARY",
            fake_claude(bin.path(), &response).to_str().unwrap(),
        );

        let mut args = base_args();
        args.repo = Some(repo.path().to_path_buf());
        args.runner = Some(crate::cli::args::RunnerChoice::ClaudeCli);
        let raw = serde_json::json!({"session_id": "sess-impl-1"}).to_string();
        exec_hook_with(&args, &raw);

        let session = governance_session::load("sess-impl-1", &rules);
        assert_eq!(
            session.deny_counts.get(&governance_session::key(
                "cross-cutting-token-signing-1c57",
                "R-A-002"
            )),
            Some(&1)
        );
    }

    /// The acceptance criterion from the module doc: a `session_id` no prior
    /// `plan-check` session has ever touched must still produce a real
    /// verdict, not bootstrap-fail — `GovernanceSession::default()` starts
    /// the loop the same way any brand-new session does.
    #[cfg(unix)]
    #[test]
    fn test_exec_hook_with_produces_a_verdict_for_a_session_id_no_plan_check_session_ever_used() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let repo = git_repo_with_baseline();
        let rules = repo.path().join(".actual/rules");
        std::fs::create_dir_all(&rules).unwrap();
        std::fs::write(rules.join("cross-cutting-token-signing-1c57.md"), OAUTH_DOC).unwrap();
        std::fs::write(
            repo.path().join("oauth.rs"),
            "fn sign() { sign_with_rs256(); }\n",
        )
        .unwrap();

        let bin = tempdir().unwrap();
        let response = check_output(serde_json::json!([
            {"doc_slug": "cross-cutting-token-signing-1c57", "rule_id": "R-A-001", "verdict": "conforming", "span": "", "reason": "uses RS256"},
            {"doc_slug": "cross-cutting-token-signing-1c57", "rule_id": "R-A-002", "verdict": "conforming", "span": "", "reason": "no logging"},
        ]));
        let _binary = EnvGuard::set(
            "CLAUDE_BINARY",
            fake_claude(bin.path(), &response).to_str().unwrap(),
        );

        // A session id that is guaranteed to be brand-new: no `plan-check`
        // call (or anything else) has ever stored state under it.
        let novel_session_id = "sess-impl-check-never-seen-before-anywhere";
        assert_eq!(
            governance_session::load(novel_session_id, &rules),
            GovernanceSession::default(),
            "sanity check: this session id must start with no prior state"
        );

        let mut args = base_args();
        args.repo = Some(repo.path().to_path_buf());
        args.runner = Some(crate::cli::args::RunnerChoice::ClaudeCli);
        let raw = serde_json::json!({"session_id": novel_session_id}).to_string();
        exec_hook_with(&args, &raw);

        // A verdict was reached and persisted -- proof the pipeline actually
        // ran to completion rather than failing to bootstrap.
        let session = governance_session::load(novel_session_id, &rules);
        assert_eq!(session.rounds, 1);
        assert!(session.cleared.contains_key(&governance_session::key(
            "cross-cutting-token-signing-1c57",
            "R-A-001"
        )));
    }

    #[cfg(all(unix, feature = "telemetry"))]
    #[test]
    fn test_exec_hook_with_deny_emits_block_governance_events_with_impl_check_command() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let repo = git_repo_with_baseline();
        let rules = repo.path().join(".actual/rules");
        std::fs::create_dir_all(&rules).unwrap();
        std::fs::write(rules.join("cross-cutting-token-signing-1c57.md"), OAUTH_DOC).unwrap();
        std::fs::write(
            repo.path().join("oauth.rs"),
            "fn sign() { log_signing_key(); }\n",
        )
        .unwrap();

        let bin = tempdir().unwrap();
        let response = check_output(serde_json::json!([
            {"doc_slug": "cross-cutting-token-signing-1c57", "rule_id": "R-A-001", "verdict": "conforming", "span": "", "reason": "uses RS256"},
            {"doc_slug": "cross-cutting-token-signing-1c57", "rule_id": "R-A-002", "verdict": "conflicting", "span": "logs the key", "reason": "forbidden"},
        ]));
        let _binary = EnvGuard::set(
            "CLAUDE_BINARY",
            fake_claude(bin.path(), &response).to_str().unwrap(),
        );

        let mut args = base_args();
        args.repo = Some(repo.path().to_path_buf());
        args.runner = Some(crate::cli::args::RunnerChoice::ClaudeCli);
        let raw = serde_json::json!({"session_id": "sess-impl-telemetry-1"}).to_string();

        let events = with_captured_plan_governance_events(|| exec_hook_with(&args, &raw));

        let completed = events
            .iter()
            .find(|e| {
                e.event == crate::api::types::PlanGovernanceEventName::PlanGovernanceCheckCompleted
            })
            .expect("a completed event must be emitted");
        let props = completed.properties.as_ref().unwrap();
        assert_eq!(
            props.decision,
            Some(crate::api::types::PlanGovernanceDecision::Block)
        );
        assert_eq!(
            props.command.as_deref(),
            Some("impl-check --claude-hook"),
            "impl-check's hook telemetry must not be mislabeled as plan-check's"
        );
    }

    /// The revision loop, end to end: a denied round, an override clearing
    /// the denied rule, and a re-run that no longer blocks on it.
    #[cfg(unix)]
    #[test]
    fn test_exec_hook_with_honors_an_override_recorded_via_check_override() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let repo = git_repo_with_baseline();
        let rules = repo.path().join(".actual/rules");
        std::fs::create_dir_all(&rules).unwrap();
        std::fs::write(rules.join("cross-cutting-token-signing-1c57.md"), OAUTH_DOC).unwrap();
        std::fs::write(
            repo.path().join("oauth.rs"),
            "fn sign() { log_signing_key(); }\n",
        )
        .unwrap();

        governance_session::record_override(
            "sess-impl-override-1",
            &rules,
            &[
                governance_session::key("cross-cutting-token-signing-1c57", "R-A-001"),
                governance_session::key("cross-cutting-token-signing-1c57", "R-A-002"),
            ],
            "reviewed and accepted by the security team",
        );

        // No working runner at all: if the override did not exclude both
        // rules, run_pipeline would need to resolve one and this would
        // surface as NoRunner instead of completing silently.
        let _no_claude = EnvGuard::set("CLAUDE_BINARY", "/nonexistent/path/to/claude");

        let mut args = base_args();
        args.repo = Some(repo.path().to_path_buf());
        args.runner = Some(crate::cli::args::RunnerChoice::ClaudeCli);
        let raw = serde_json::json!({"session_id": "sess-impl-override-1"}).to_string();
        exec_hook_with(&args, &raw);

        let session = governance_session::load("sess-impl-override-1", &rules);
        assert_eq!(session.overrides.len(), 2);
    }
}
