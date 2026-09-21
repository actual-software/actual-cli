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
//! one — so `--claude-hook` mode here always shells out to
//! [`working_tree_diff`] in the resolved repository. Direct mode additionally
//! accepts `--diff-file` (for scripting/testing) and piped stdin, in that
//! priority order, falling back to [`working_tree_diff`] when neither is
//! given and stdin is not an explicit diff source — a TTY, or a non-terminal
//! character device such as `/dev/null` (CI, nohup, a hook with no pipe).
//! Those two are not pipes; treating them as empty stdin would skip the
//! working-tree diff and report "nothing to check" on a dirty tree. An empty
//! *pipe* is still "nothing to check," same as an empty `--diff-file` — see
//! [`resolve_direct_diff`].
//!
//! **Untracked files are part of the default diff.** Plain `git diff HEAD`
//! only shows tracked-file changes, so an agent that only creates files
//! would produce an empty diff and skip the gate. [`working_tree_diff`]
//! copies `HEAD` into a throwaway index, `git add --intent-to-add` against
//! that index (gitignore still applies), then `git diff HEAD` — the user's
//! real index is never touched. `--diff-file` and piped stdin are unchanged.
//!
//! **An empty diff is not a user error.** `plan-check`'s direct mode refuses
//! an empty plan outright (`resolve_direct_plan` returns
//! `ActualError::ConfigError`): a plan is something a human or agent is
//! expected to have written, so nothing at all is almost always a mistake. A
//! diff is different — the working-tree diff is legitimately empty the
//! moment the working tree matches `HEAD` and there are no untracked
//! (non-ignored) files, which is an entirely ordinary state (freshly cloned,
//! freshly committed, nothing touched yet), not a missing argument. So an
//! empty diff from *any* source here (including an explicitly empty
//! `--diff-file` or empty piped stdin, for the same uniform-treatment
//! reason) is handled as "nothing to check": direct mode
//! prints a clean, informational result and exits 0 rather than erroring,
//! and `--claude-hook` mode emits a non-blocking notice and returns, mirroring
//! exactly how `plan_check_hook::resolve_plan` returning `None` is handled in
//! `plan_check::exec_hook_with`.
//!
//! **The revision loop needs no plan-check bootstrap.** `GovernanceSession`
//! is still one file per `(session_id, rules_dir)` so a human override
//! recorded against the conversation applies to both gates, but deny counts,
//! rounds, and clearances are per [`ArtifactKind`] — `impl-check --claude-hook`
//! run with a `session_id` no `plan-check` session has ever touched simply
//! starts from `GovernanceSession::default()`, and a session that *has* run
//! plan-check still starts impl-check with a fresh deny budget. There is
//! nothing here to wire up for that to work.

use std::io::{IsTerminal, Read};
use std::path::Path;

use crate::cli::args::ImplCheckArgs;
use crate::cli::commands::check_engine::{
    self, capped_read, deny_summary, emit, finish_hook, render_json, render_panel, run_pipeline,
    HookRun, Outcome,
};
use crate::cli::commands::governance_session::{self, GovernanceSession};
use crate::cli::commands::impl_check_hook::HookEnvelope;
use crate::cli::commands::plan_check::repo_root;
use crate::cli::commands::plan_check_hook;
use crate::cli::ui::term_size;
use crate::error::ActualError;
#[cfg(feature = "telemetry")]
use crate::rules::check::Verdict;
use crate::rules::check::{ArtifactKind, CheckedRule};

#[cfg(feature = "telemetry")]
use crate::cli::commands::check_engine::send_governance_events;

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
        println!("{}", render_json(&outcome, ArtifactKind::Diff));
    } else {
        println!(
            "{}",
            render_panel(&outcome, &diff_text, &rules_dir, width, ArtifactKind::Diff)
        );
    }

    let result = if let Outcome::Verdicts { verdicts, .. } = &outcome {
        let conflicts: Vec<&CheckedRule> = verdicts.iter().filter(|v| v.verdict.blocks()).collect();
        if !conflicts.is_empty() {
            Err(ActualError::ImplNotConforming(deny_summary(
                &conflicts,
                ArtifactKind::Diff,
            )))
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

/// The diff text for direct-mode use: `--diff-file`, then an explicit stdin
/// source (a pipe or redirected file), then [`working_tree_diff`] in the
/// resolved repo root.
///
/// Stdin is *not* an explicit source just because it fails
/// [`IsTerminal`]. `/dev/null` (and other non-terminal character devices)
/// also fail that check, and reading them yields immediate EOF — which this
/// command would otherwise treat as an empty diff and skip the working-tree
/// diff, a silent pass in CI, nohup, and hooks that attach stdin to
/// `/dev/null`. A real pipe or redirected file is not a character device;
/// those still count as the diff, including an empty one. A TTY is skipped
/// so an interactive run does not block waiting for a keyboard that will
/// never supply a diff.
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
    if stdin_is_explicit_diff_source() {
        let text = capped_read(std::io::stdin(), "stdin")?;
        return Ok(non_empty(text));
    }
    Ok(non_empty(working_tree_diff(root)?))
}

/// True when stdin is a pipe or redirected file the caller supplied as the
/// diff. False for a TTY and for a non-terminal character device (`/dev/null`
/// in CI): neither is an explicit diff, so [`resolve_direct_diff`] falls
/// through to [`working_tree_diff`].
fn stdin_is_explicit_diff_source() -> bool {
    is_explicit_diff_source(std::io::stdin().is_terminal(), stdin_is_char_device())
}

fn is_explicit_diff_source(is_terminal: bool, is_char_device: bool) -> bool {
    !is_terminal && !is_char_device
}

/// Whether stdin itself is a character device. A TTY is one; so is
/// `/dev/null`. Callers that already know stdin is not a TTY use this to
/// tell "no input attached" apart from a pipe.
fn stdin_is_char_device() -> bool {
    #[cfg(unix)]
    {
        use std::mem::ManuallyDrop;
        use std::os::fd::AsRawFd;
        use std::os::unix::fs::FileTypeExt;
        use std::os::unix::io::FromRawFd;

        let fd = std::io::stdin().as_raw_fd();
        // SAFETY: stdin stays open for the process lifetime. `ManuallyDrop`
        // keeps `File`'s destructor from closing that fd after `metadata`.
        let file = ManuallyDrop::new(unsafe { std::fs::File::from_raw_fd(fd) });
        file.metadata()
            .map(|m| m.file_type().is_char_device())
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        false
    }
}

fn non_empty(text: String) -> Option<String> {
    if text.trim().is_empty() {
        None
    } else {
        Some(text)
    }
}

/// The default diff: working tree vs `HEAD`, including untracked files that
/// gitignore would not hide.
///
/// Plain `git diff HEAD` cannot see new files, and an implementation gate
/// that misses those is a hole — agents create files more often than they
/// edit tracked ones. Copying `HEAD` into a throwaway `GIT_INDEX_FILE` and
/// `git add --intent-to-add` against *that* index makes those files show up
/// as new-file hunks without touching the user's real index (a crash here
/// must not leave `git add -N` entries behind in the repo they are checking).
///
/// Capped at the same size limit every other diff source is capped at
/// ([`plan_check_hook::MAX_READ_BYTES`], via [`capped_read`] on the child's
/// own piped stdout). Synchronous, deliberately: this is a local git
/// operation with no network involved, unlike this repo's async git-remote
/// calls in `advisor.rs` / `sync/cache.rs`.
fn working_tree_diff(root: &Path) -> Result<String, ActualError> {
    let tmp = tempfile::tempdir().map_err(ActualError::IoError)?;
    let index = tmp.path().join("index");
    for (args, what) in [
        (&["read-tree", "HEAD"][..], "git read-tree HEAD"),
        (
            &["add", "--intent-to-add", "--", "."][..],
            "git add --intent-to-add",
        ),
    ] {
        git_ok(root, Some(&index), args, what)?;
    }

    let mut child = git_command(root, Some(&index))
        .args(["diff", "--no-ext-diff", "HEAD"])
        .spawn()
        .map_err(ActualError::IoError)?;
    let stdout = child.stdout.take().expect("stdout was piped");
    let text = match capped_read(stdout, "working-tree diff") {
        Ok(text) => text,
        Err(e) => {
            // Over the cap (or unreadable): git may still be writing. Stop it
            // and reap it rather than leaving a zombie behind the error.
            let _ = child.kill();
            let _ = child.wait();
            return Err(e);
        }
    };
    let mut stderr = String::new();
    if let Some(mut err) = child.stderr.take() {
        let _ = err.read_to_string(&mut stderr);
    }
    let status = child.wait().map_err(ActualError::IoError)?;
    if !status.success() {
        return Err(ActualError::ConfigError(format!(
            "working-tree diff failed in {}: {}",
            root.display(),
            stderr.trim()
        )));
    }
    Ok(text)
}

/// `--no-pager` and `color.ui=never` keep the captured text free of a pager
/// and ANSI escapes even when the user's config says `color.ui=always`.
/// (`--no-ext-diff` is a `diff` option, so it lives on the `diff` call.)
fn git_command(root: &Path, index: Option<&Path>) -> std::process::Command {
    let mut cmd = std::process::Command::new("git");
    cmd.args(["--no-pager", "-c", "color.ui=never"])
        .current_dir(root)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    if let Some(index) = index {
        cmd.env("GIT_INDEX_FILE", index);
    }
    cmd
}

fn git_ok(root: &Path, index: Option<&Path>, args: &[&str], what: &str) -> Result<(), ActualError> {
    let output = git_command(root, index)
        .args(args)
        .output()
        .map_err(ActualError::IoError)?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(ActualError::ConfigError(format!(
            "{what} failed in {}: {}",
            root.display(),
            stderr.trim()
        )));
    }
    Ok(())
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
/// Only resolving the diff and running the pipeline live here; everything
/// after the pipeline's outcome (session bookkeeping, deny, round limit,
/// notices, telemetry) is `check_engine::finish_hook`, shared with
/// `plan-check` so the two gates cannot drift.
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
    // the working-tree diff in the resolved repo. See the module doc.
    let diff_text = match working_tree_diff(&root) {
        Ok(text) => text,
        Err(e) => {
            emit(plan_check_hook::render_notice(&format!(
                "impl-check could not read the working-tree diff in {}: {e}",
                root.display()
            )));
            return;
        }
    };
    if diff_text.trim().is_empty() {
        emit(plan_check_hook::render_notice(
            "impl-check found no diff to check (working tree matches HEAD and there are no untracked files).",
        ));
        return;
    }

    // The revision loop keys on `(session_id, rules_dir)` for the file, then
    // on `ArtifactKind::Diff` for deny counts / rounds / clearances — a
    // session no prior `impl-check` call has ever touched starts this loop
    // fresh, even if `plan-check` already wrote overrides into the same file.
    // See the module doc.
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

    finish_hook(
        &HookRun {
            kind: ArtifactKind::Diff,
            command: "impl-check --claude-hook",
            session_id,
            artifact_digest: &diff_digest,
            root: &root,
            rules_dir: &rules_dir,
            max_rounds: args.max_rounds,
            #[cfg(feature = "telemetry")]
            started_at,
        },
        &mut session,
        outcome,
    );
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

    fn git_stdout(cwd: &StdPath, git_args: &[&str]) -> String {
        let output = std::process::Command::new("git")
            .args(git_args)
            .current_dir(cwd)
            .output()
            .expect("git is available in the test environment");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "git {git_args:?} failed: {stderr}");
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    /// A real throwaway git repo with a committed baseline, so a working-tree
    /// diff has `HEAD` to compare against. `oauth.rs` is committed at
    /// baseline so tracked-edit tests can overwrite it; untracked files are
    /// included too (see [`working_tree_diff`]), and those tests write a
    /// brand-new file instead of modifying this fixture.
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

    // ── resolve_direct_diff / working_tree_diff ──────────────────────────

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
    fn test_working_tree_diff_reads_a_real_working_tree_change() {
        let repo = git_repo_with_baseline();
        std::fs::write(repo.path().join("service.rs"), "fn handler() { todo!() }\n").unwrap();
        let diff = working_tree_diff(repo.path()).unwrap();
        assert!(diff.contains("service.rs"));
        assert!(diff.contains("todo!()"));
    }

    #[test]
    fn test_working_tree_diff_includes_an_untracked_new_file() {
        let repo = git_repo_with_baseline();
        std::fs::write(
            repo.path().join("brand_new.rs"),
            "fn freshly_created() {}\n",
        )
        .unwrap();
        let diff = working_tree_diff(repo.path()).unwrap();
        assert!(
            diff.contains("brand_new.rs"),
            "untracked files must appear in the default diff, got: {diff}"
        );
        assert!(diff.contains("freshly_created"));
    }

    #[test]
    fn test_working_tree_diff_omits_a_gitignored_untracked_file() {
        let repo = git_repo_with_baseline();
        std::fs::write(repo.path().join(".gitignore"), "ignored.rs\n").unwrap();
        std::fs::write(repo.path().join("ignored.rs"), "fn secret() {}\n").unwrap();
        std::fs::write(repo.path().join("visible.rs"), "fn ok() {}\n").unwrap();
        let diff = working_tree_diff(repo.path()).unwrap();
        assert!(diff.contains("visible.rs"));
        assert!(
            !diff.contains("+++ b/ignored.rs"),
            "gitignored untracked files must stay out of the default diff, got: {diff}"
        );
        assert!(!diff.contains("fn secret()"));
    }

    #[test]
    fn test_working_tree_diff_does_not_mutate_the_real_index() {
        let repo = git_repo_with_baseline();
        std::fs::write(
            repo.path().join("service.rs"),
            "fn handler() { staged() }\n",
        )
        .unwrap();
        run_git(repo.path(), &["add", "service.rs"]);
        std::fs::write(repo.path().join("brand_new.rs"), "fn untracked() {}\n").unwrap();

        let cached_before = git_stdout(repo.path(), &["diff", "--cached"]);
        let staged_before = git_stdout(repo.path(), &["ls-files", "--stage"]);
        assert!(
            cached_before.contains("staged()"),
            "precondition: the real index must hold the staged edit"
        );

        let diff = working_tree_diff(repo.path()).unwrap();
        assert!(diff.contains("brand_new.rs"));
        assert!(diff.contains("staged()"));

        assert_eq!(
            git_stdout(repo.path(), &["diff", "--cached"]),
            cached_before,
            "working_tree_diff must not change git diff --cached"
        );
        assert_eq!(
            git_stdout(repo.path(), &["ls-files", "--stage"]),
            staged_before,
            "working_tree_diff must not change the real index"
        );
        let status = git_stdout(repo.path(), &["status", "--porcelain"]);
        assert!(
            status.contains("?? brand_new.rs"),
            "the new file must still be untracked in the real repo, got: {status}"
        );
    }

    #[test]
    fn test_working_tree_diff_empty_when_nothing_changed() {
        let repo = git_repo_with_baseline();
        let diff = working_tree_diff(repo.path()).unwrap();
        assert_eq!(diff.trim(), "");
    }

    #[test]
    fn test_working_tree_diff_errors_outside_a_git_repository() {
        let not_a_repo = tempdir().unwrap();
        let err = working_tree_diff(not_a_repo.path()).unwrap_err();
        assert!(matches!(err, ActualError::ConfigError(_)));
    }

    /// The cap is on the diff, not just on `--diff-file`/stdin: a working tree
    /// whose diff exceeds it must error rather than buffer it whole. This also
    /// exercises the kill-and-reap path, so a regression that left `git`
    /// blocked writing into a pipe nobody reads would hang this test.
    #[test]
    fn test_working_tree_diff_errors_when_the_diff_exceeds_the_size_limit() {
        let repo = git_repo_with_baseline();
        let oversized = "x".repeat(plan_check_hook::MAX_READ_BYTES as usize + 1);
        std::fs::write(repo.path().join("huge.txt"), oversized).unwrap();

        let err = working_tree_diff(repo.path()).unwrap_err();

        assert!(matches!(err, ActualError::ConfigError(_)), "{err:?}");
        let msg = err.to_string();
        assert!(msg.contains("working-tree diff"), "{msg}");
        assert!(msg.contains("exceeds"), "{msg}");
    }

    /// `git diff` itself exiting non-zero (here: HEAD's copy of a modified
    /// file is missing from the object store) is reported with git's own
    /// stderr, not swallowed into an empty diff that would pass the gate.
    #[cfg(unix)]
    #[test]
    fn test_working_tree_diff_errors_when_git_diff_exits_nonzero() {
        let repo = git_repo_with_baseline();
        let blob = git_stdout(repo.path(), &["rev-parse", "HEAD:service.rs"]);
        let blob = blob.trim();
        let object = repo
            .path()
            .join(".git/objects")
            .join(&blob[..2])
            .join(&blob[2..]);
        std::fs::remove_file(object).unwrap();
        std::fs::write(repo.path().join("service.rs"), "fn handler() { 5 }\n").unwrap();

        let err = working_tree_diff(repo.path()).unwrap_err();

        assert!(
            err.to_string().contains("working-tree diff failed in"),
            "{err}"
        );
    }

    #[test]
    fn test_is_explicit_diff_source_skips_tty_and_char_devices() {
        assert!(
            !is_explicit_diff_source(true, false),
            "a TTY is not an explicit diff — fall through to git diff HEAD"
        );
        assert!(
            !is_explicit_diff_source(true, true),
            "a TTY is a char device too; still not an explicit diff"
        );
        assert!(
            !is_explicit_diff_source(false, true),
            "/dev/null is a non-TTY char device — fall through to git diff HEAD"
        );
        assert!(
            is_explicit_diff_source(false, false),
            "a pipe or redirected file is an explicit diff source"
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_dev_null_is_a_character_device() {
        use std::os::unix::fs::FileTypeExt;
        let meta = std::fs::metadata("/dev/null").unwrap();
        assert!(
            meta.file_type().is_char_device(),
            "the /dev/null case in stdin_is_char_device depends on this"
        );
    }

    #[test]
    fn test_resolve_direct_diff_falls_back_to_working_tree_diff_with_no_file_and_a_terminal_stdin()
    {
        // `IsTerminal` and the stdin fd's file type cannot be faked from an
        // in-process test the same way `check_engine::exec_override`'s own
        // doc explains -- this exercises the fallback shape indirectly, via
        // `working_tree_diff` itself. The subprocess-driven integration test
        // (`tests/cli_test.rs`) covers `/dev/null` vs an empty pipe, which
        // *is* controllable there.
        let repo = git_repo_with_baseline();
        std::fs::write(repo.path().join("service.rs"), "fn handler() { 1 }\n").unwrap();
        let diff = working_tree_diff(repo.path()).unwrap();
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

    /// `--json` on an empty diff is the same clean, exit-0 "not checked"
    /// result as the panel, just machine-readable.
    #[test]
    fn test_exec_direct_json_prints_not_checked_on_an_empty_diff() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let repo = git_repo_with_baseline();
        // An explicit empty `--diff-file`, not the fallback chain: that would
        // consult the real stdin, which a test harness does not control.
        let diff_dir = tempdir().unwrap();
        let diff_file = diff_dir.path().join("empty.diff");
        std::fs::write(&diff_file, "  \n").unwrap();
        let mut args = base_args();
        args.diff_file = Some(diff_file);
        args.repo = Some(repo.path().to_path_buf());
        args.json = true;
        assert!(exec(&args).is_ok());
    }

    /// A `requires_decision` verdict does not fail direct mode (only a real
    /// conflict does), but it is not a clean pass either: `--json` reports it
    /// and the telemetry decision is `warn`.
    #[cfg(unix)]
    #[test]
    fn test_exec_direct_json_requires_decision_exits_ok_and_warns() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let root = seed(&[("cross-cutting-token-signing-1c57.md", OAUTH_DOC)]);
        let bin = tempdir().unwrap();
        let response = check_output(serde_json::json!([
            {"doc_slug": "cross-cutting-token-signing-1c57", "rule_id": "R-A-001", "verdict": "conforming", "span": "", "reason": "uses RS256"},
            {"doc_slug": "cross-cutting-token-signing-1c57", "rule_id": "R-A-002", "verdict": "requires_decision", "span": "logs the key", "reason": "deliberately supersedes the rule"},
        ]));
        let _binary = EnvGuard::set(
            "CLAUDE_BINARY",
            fake_claude(bin.path(), &response).to_str().unwrap(),
        );

        let diff_dir = tempdir().unwrap();
        let diff_file = diff_dir.path().join("the.diff");
        std::fs::write(
            &diff_file,
            "diff --git a/oauth.rs b/oauth.rs\n+log the signing key\n",
        )
        .unwrap();

        let mut args = base_args();
        args.diff_file = Some(diff_file);
        args.repo = Some(root.path().to_path_buf());
        args.runner = Some(crate::cli::args::RunnerChoice::ClaudeCli);
        args.no_rank = true;
        args.json = true;

        #[cfg(feature = "telemetry")]
        {
            let events = with_captured_plan_governance_events(|| assert!(exec(&args).is_ok()));
            let completed = events
                .iter()
                .find(|e| {
                    e.event
                        == crate::api::types::PlanGovernanceEventName::PlanGovernanceCheckCompleted
                })
                .expect("a completed event must be emitted");
            assert_eq!(
                completed.properties.as_ref().unwrap().decision,
                Some(crate::api::types::PlanGovernanceDecision::Warn)
            );
        }
        #[cfg(not(feature = "telemetry"))]
        assert!(exec(&args).is_ok());
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

    /// A user's `color.ui=always` and `diff.external` must not reach the text
    /// the judge reads: ANSI escapes and a non-unified external diff would
    /// both corrupt it.
    #[cfg(unix)]
    #[test]
    fn test_working_tree_diff_ignores_color_and_external_diff_config() {
        use std::os::unix::fs::PermissionsExt;

        let repo = git_repo_with_baseline();
        let bin = tempdir().unwrap();
        let ext = bin.path().join("ext-diff.sh");
        std::fs::write(&ext, "#!/bin/sh\necho EXTERNAL-DIFF-OUTPUT\n").unwrap();
        std::fs::set_permissions(&ext, std::fs::Permissions::from_mode(0o755)).unwrap();
        run_git(repo.path(), &["config", "color.ui", "always"]);
        run_git(
            repo.path(),
            &["config", "diff.external", ext.to_str().unwrap()],
        );
        std::fs::write(repo.path().join("service.rs"), "fn handler() { 4 }\n").unwrap();

        let diff = working_tree_diff(repo.path()).unwrap();

        assert!(
            diff.contains("diff --git a/service.rs b/service.rs"),
            "{diff}"
        );
        assert!(!diff.contains("EXTERNAL-DIFF-OUTPUT"), "{diff}");
        assert!(!diff.contains('\u{1b}'), "no ANSI escapes: {diff:?}");
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
            session.diff.deny_counts.get(&governance_session::key(
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
        assert_eq!(session.diff.rounds, 1);
        assert!(session.diff.cleared.contains_key(&governance_session::key(
            "cross-cutting-token-signing-1c57",
            "R-A-001"
        )));
    }

    /// An untracked new file, with no tracked-file edits, must still reach
    /// the judge — the hole `working_tree_diff` exists to close. Rules are
    /// committed first so they are not themselves the untracked material
    /// that makes the diff non-empty.
    #[cfg(unix)]
    #[test]
    fn test_exec_hook_with_judges_an_untracked_new_file() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let repo = git_repo_with_baseline();
        let rules = repo.path().join(".actual/rules");
        std::fs::create_dir_all(&rules).unwrap();
        std::fs::write(rules.join("cross-cutting-token-signing-1c57.md"), OAUTH_DOC).unwrap();
        run_git(repo.path(), &["add", "."]);
        run_git(repo.path(), &["commit", "-q", "-m", "rules"]);
        std::fs::write(
            repo.path().join("brand_new.rs"),
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

        let mut args = base_args();
        args.repo = Some(repo.path().to_path_buf());
        args.runner = Some(crate::cli::args::RunnerChoice::ClaudeCli);
        let raw = serde_json::json!({"session_id": "sess-impl-untracked-1"}).to_string();
        exec_hook_with(&args, &raw);

        let session = governance_session::load("sess-impl-untracked-1", &rules);
        assert_eq!(
            session.diff.rounds, 1,
            "an untracked-only change must be judged, not skipped as an empty diff"
        );
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

    /// The gap this guards: plan-stage denials used to live in the same
    /// `deny_counts` map impl-check read, so an exhausted plan-check budget
    /// made the first impl-check of that rule fail open.
    #[cfg(unix)]
    #[test]
    fn test_exec_hook_with_plan_denials_do_not_exhaust_impl_check_budget() {
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

        let key = governance_session::key("cross-cutting-token-signing-1c57", "R-A-002");
        let mut prior = GovernanceSession::default();
        for _ in 0..4 {
            prior.record_denial(ArtifactKind::Plan, &key);
        }
        prior.plan.rounds = 4;
        governance_session::store("sess-impl-budget-1", &rules, &prior);
        assert!(prior.deny_limit_exceeded(ArtifactKind::Plan, &key, DEFAULT_MAX_ROUNDS));
        assert!(!prior.deny_limit_exceeded(ArtifactKind::Diff, &key, DEFAULT_MAX_ROUNDS));

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
        let raw = serde_json::json!({"session_id": "sess-impl-budget-1"}).to_string();
        exec_hook_with(&args, &raw);

        let session = governance_session::load("sess-impl-budget-1", &rules);
        assert_eq!(session.plan.deny_counts.get(&key), Some(&4));
        assert_eq!(session.diff.deny_counts.get(&key), Some(&1));
        assert!(
            !session.deny_limit_exceeded(ArtifactKind::Diff, &key, args.max_rounds),
            "the first impl-check denial must not already be exhausted"
        );
        let log = std::fs::read_to_string(governance_session::audit_log_path().unwrap())
            .unwrap_or_default();
        assert!(
            !log.contains("round_limit"),
            "a plan-exhausted rule must still deny at impl-check: {log}"
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

        // A working runner that would *deny* R-A-001 if it were ever asked. If
        // the override failed to exclude the rule, the judge runs and the
        // denial is recorded; a missing binary could not tell the two apart,
        // since a failed exclusion would just fail open as `NoRunner`.
        let bin = tempdir().unwrap();
        let response = check_output(serde_json::json!([
            {"doc_slug": "cross-cutting-token-signing-1c57", "rule_id": "R-A-001", "verdict": "conflicting", "span": "logs the key", "reason": "forbidden"},
        ]));
        let _binary = EnvGuard::set(
            "CLAUDE_BINARY",
            fake_claude(bin.path(), &response).to_str().unwrap(),
        );

        let mut args = base_args();
        args.repo = Some(repo.path().to_path_buf());
        args.runner = Some(crate::cli::args::RunnerChoice::ClaudeCli);
        let raw = serde_json::json!({"session_id": "sess-impl-override-1"}).to_string();
        exec_hook_with(&args, &raw);

        let session = governance_session::load("sess-impl-override-1", &rules);
        assert_eq!(session.overrides.len(), 2);
        assert_eq!(
            session.diff.rounds, 0,
            "both rules are overridden, so no judge round should have run"
        );
        assert!(
            session.diff.deny_counts.is_empty(),
            "an overridden rule must not be denied: {:?}",
            session.diff.deny_counts
        );
    }
}
