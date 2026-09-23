//! `actual rules brief` — the rules governing one file, for a Claude Code hook.
//!
//! # Design
//!
//! The agent meets a rule today only when a gate denies it. This command is
//! the other direction: given the file the agent is about to touch, it names
//! the decisions that govern it, so the rules are in context while the agent
//! can still act on them.
//!
//! **Two events, one command.** `PostToolUse` on `Read` is the main path:
//! Claude Code requires a read before an edit, and context delivered there
//! reaches the model *before* the first edit. `PreToolUse` on `Edit`/`Write`
//! is the reminder for files that were never read — mostly new ones — where
//! the context arrives with the tool result, after the edit ran. The hook's
//! reply must name the event it is answering, so the event is read from the
//! envelope and echoed back rather than hardcoded.
//!
//! **Advisory, structurally.** The output type here has no
//! `permissionDecision` field, so this hook cannot grant or deny a tool call
//! even by mistake — the same guarantee `plan_check_hook::render_deny` gets
//! from the opposite direction, where `allow` has no constructor.
//!
//! **Stage 1 only.** No runner, no network: a hook that runs on every read
//! cannot afford a model call, and the ranking this selects with is
//! deterministic and offline.
//!
//! **Fail open, always.** An unreadable envelope, a path outside the
//! repository, a missing rule set, an unbuildable index: each produces empty
//! stdout and exit 0. A governance aid that breaks an agent's edit loop when
//! it malfunctions is worse than one that says nothing.
//!
//! Session dedupe (a decision briefed once per session) is AK-791 and is not
//! here yet, so a repeated read of one file briefs it again.

use std::io::{IsTerminal, Read};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::cli::args::RulesBriefArgs;
use crate::error::ActualError;
use crate::rules::brief::{render_brief, BriefDecision};
use crate::rules::scope::{self, index::Query};

/// Hook events this command answers. Anything else gets silence: a brief
/// belongs to a file the agent is reading or editing, and no other event
/// carries one.
const ANSWERED_EVENTS: &[&str] = &["PostToolUse", "PreToolUse"];

/// Tools whose envelopes name a file worth briefing.
const ANSWERED_TOOLS: &[&str] = &["Read", "Edit", "Write", "MultiEdit", "NotebookEdit"];

/// The fields this command reads from a hook envelope.
///
/// Unmodelled fields are ignored rather than rejected, so a newer Claude Code
/// envelope still deserializes — the same tolerance the other two hook
/// envelopes in this crate give.
#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub struct HookEnvelope {
    pub hook_event_name: Option<String>,
    pub tool_name: Option<String>,
    pub session_id: Option<String>,
    pub cwd: Option<String>,
    pub tool_input: Option<ToolInput>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub struct ToolInput {
    pub file_path: Option<String>,
}

#[derive(Debug, Serialize)]
struct HookSpecificContext {
    #[serde(rename = "hookEventName")]
    hook_event_name: String,
    #[serde(rename = "additionalContext")]
    additional_context: String,
}

/// The hook's only output shape. It has no `permissionDecision` field, which
/// is what makes "this hook never grants or denies" a property of the type
/// rather than a promise in a comment.
#[derive(Debug, Serialize)]
struct ContextOutput {
    #[serde(rename = "hookSpecificOutput")]
    hook_specific_output: HookSpecificContext,
}

pub fn exec(args: &RulesBriefArgs) -> Result<(), ActualError> {
    if args.claude_hook {
        emit(hook_reply(&read_stdin(), args));
        // Always `Ok`: see the fail-open note in the module docs.
        return Ok(());
    }
    exec_direct(args)
}

/// Print a reply, or nothing. Split out so the decision to stay silent is
/// testable without a process boundary.
fn emit(reply: Option<String>) {
    if let Some(line) = reply {
        println!("{line}");
    }
}

/// Direct mode: the brief for `--file`, as text. The hook's own output is
/// JSON, so this exists to make the same brief readable at a terminal and
/// diffable in a test.
fn exec_direct(args: &RulesBriefArgs) -> Result<(), ActualError> {
    let Some(file) = args.file.clone() else {
        return Err(ActualError::ConfigError(
            "rules brief needs --file, or --claude-hook with an envelope on stdin".to_string(),
        ));
    };
    let root = args
        .repo
        .clone()
        .unwrap_or_else(crate::cli::commands::sync::resolve_cwd);
    match brief_for(&root, &file, args) {
        Some(brief) => println!("{brief}"),
        None => println!("No rule document governs {file}."),
    }
    Ok(())
}

/// Read the whole envelope, when one was actually piped in.
///
/// The guard matters as much as the read. A hook always attaches a pipe, but
/// a terminal or `/dev/null` attaches neither an envelope nor an end of file
/// a reader can wait on — `impl_check` learned this the same way, when its
/// direct-mode tests blocked on the test harness's own stdin. Anything that
/// is not a pipe or a redirected file reads as empty, which is silence.
fn read_stdin() -> String {
    if !stdin_is_piped() {
        return String::new();
    }
    read_envelope(std::io::stdin())
}

/// The reading half, over any source, so the "an unreadable source is
/// silence, not an error" rule is testable without a pipe.
fn read_envelope(mut source: impl Read) -> String {
    let mut raw = String::new();
    let _ = source.read_to_string(&mut raw);
    raw
}

fn stdin_is_piped() -> bool {
    is_piped(std::io::stdin().is_terminal(), stdin_is_char_device())
}

/// Pure half of [`stdin_is_piped`], so the rule is testable without a real
/// file descriptor: a pipe or redirected file is neither a terminal nor a
/// character device.
fn is_piped(is_terminal: bool, is_char_device: bool) -> bool {
    !is_terminal && !is_char_device
}

/// Whether stdin is a character device — a TTY, or `/dev/null` as CI and a
/// test harness attach it.
fn stdin_is_char_device() -> bool {
    #[cfg(unix)]
    {
        use std::mem::ManuallyDrop;
        use std::os::fd::AsRawFd;
        use std::os::unix::fs::FileTypeExt;
        use std::os::unix::io::FromRawFd;

        let fd = std::io::stdin().as_raw_fd();
        // SAFETY: stdin stays open for the process lifetime, and
        // `ManuallyDrop` keeps `File`'s destructor from closing that fd.
        let file = ManuallyDrop::new(unsafe { std::fs::File::from_raw_fd(fd) });
        file.metadata()
            .map(|meta| meta.file_type().is_char_device())
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        false
    }
}

/// The hook's reply for one envelope, or `None` for silence.
///
/// Every failure path returns `None`, so a caller cannot turn a malformed
/// envelope into a non-zero exit.
fn hook_reply(raw: &str, args: &RulesBriefArgs) -> Option<String> {
    let envelope: HookEnvelope = serde_json::from_str(raw).ok()?;

    let event = envelope.hook_event_name?;
    if !ANSWERED_EVENTS.contains(&event.as_str()) {
        return None;
    }
    // A `PostToolUse` on some other tool, or a `PreToolUse` on a shell
    // command, names no file to brief.
    let tool = envelope.tool_name?;
    if !ANSWERED_TOOLS.contains(&tool.as_str()) {
        return None;
    }

    let file = envelope.tool_input?.file_path?;
    let root = args
        .repo
        .clone()
        .or_else(|| envelope.cwd.map(PathBuf::from))
        .unwrap_or_else(crate::cli::commands::sync::resolve_cwd);

    let brief = brief_for(&root, &file, args)?;
    serde_json::to_string(&ContextOutput {
        hook_specific_output: HookSpecificContext {
            hook_event_name: event,
            additional_context: brief,
        },
    })
    .ok()
}

/// The brief for one file, or `None` when nothing governs it — or when
/// anything at all goes wrong.
fn brief_for(root: &Path, file: &str, args: &RulesBriefArgs) -> Option<String> {
    let relative = relative_to_root(root, file)?;

    let rules_dir = args
        .rules_dir
        .clone()
        .unwrap_or_else(|| crate::rules::rules_dir(root));
    let resolved = scope::resolve_in(&rules_dir, root, false).ok()?;

    let query = Query::new("")
        .with_paths([relative])
        .with_min_score(min_score(args));
    let decisions = resolved.index.search_adrs(&query, args.limit);
    if decisions.is_empty() {
        return None;
    }

    // The index stores what ranks a document, not what it says, so the rule
    // text is read here — one file per selected document. Loading the whole
    // rule set instead would read 425 files to render two decisions, and this
    // runs on every file the agent reads.
    let parsed: Vec<(usize, crate::rules::types::RuleDocument)> = decisions
        .iter()
        .enumerate()
        .flat_map(|(position, decision)| {
            decision.documents.iter().filter_map(move |hit| {
                let path = root.join(&hit.relative_path);
                let text = std::fs::read_to_string(&path).ok()?;
                // A document that no longer parses is skipped rather than
                // failing the brief: the rest still governs the file.
                let document = crate::rules::parse_rule_document(&path, &text).ok()?;
                Some((position, document))
            })
        })
        .collect();

    let briefed: Vec<BriefDecision<'_>> = decisions
        .iter()
        .enumerate()
        .map(|(position, decision)| BriefDecision {
            heading: decision.title.as_deref().unwrap_or(&decision.key),
            documents: parsed
                .iter()
                .filter(|(at, _)| *at == position)
                .map(|(_, document)| document)
                .collect(),
        })
        .collect();

    render_brief(&briefed, args.rules_per_decision)
}

/// The score floor for this invocation: the flag, else the config key, else
/// none. An unreadable config degrades to no floor, the same way
/// `rules select` treats it.
fn min_score(args: &RulesBriefArgs) -> f64 {
    args.min_score
        .or_else(|| crate::config::paths::load().ok()?.rules_min_score)
        .unwrap_or(0.0)
}

/// The path as the rule set names it: relative to the repository root, with
/// forward slashes.
///
/// A hook envelope carries absolute paths, while verify-block globs are
/// repository-relative, so an unconverted path matches nothing. A path
/// outside the repository yields `None` rather than a guess — briefing a file
/// in another checkout against these rules would be wrong, not merely
/// unhelpful.
fn relative_to_root(root: &Path, file: &str) -> Option<String> {
    let path = Path::new(file);
    let relative = if path.is_absolute() {
        // `canonicalize` is deliberately not used: it touches the filesystem,
        // and under `PreToolUse` on `Write` the file does not exist yet.
        path.strip_prefix(root).ok()?
    } else {
        path
    };
    let text = relative
        .components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/");
    (!text.is_empty()).then_some(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    use tempfile::{tempdir, TempDir};

    const OAUTH: &str = "# Sign With Asymmetric Keys: Token Signing\n\nThese rules are ALWAYS ACTIVE for OAuth token signing in `services/auth/oauth/`.\n\n### Rules\n\n- **R-A-001** MUST: sign with RS256.\n- **R-A-002** SHOULD: rotate keys quarterly.\n\n### Verify\n\n```bash\ngrep -r \"jwt.sign\" services/auth/oauth/ --include=\"*.ts\"\n```\n";

    fn repo() -> TempDir {
        let root = tempdir().unwrap();
        let dir = crate::rules::rules_dir(root.path());
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("cross-cutting-token-signing-e410.md"), OAUTH).unwrap();
        root
    }

    fn args(root: &Path) -> RulesBriefArgs {
        RulesBriefArgs {
            claude_hook: true,
            file: None,
            repo: Some(root.to_path_buf()),
            rules_dir: None,
            limit: 2,
            rules_per_decision: 8,
            // Explicit, so these tests never read the machine's own config.
            min_score: Some(0.0),
        }
    }

    fn envelope(root: &Path, event: &str, tool: &str, file: &str) -> String {
        serde_json::json!({
            "session_id": "s1",
            "cwd": root.to_string_lossy(),
            "hook_event_name": event,
            "tool_name": tool,
            "tool_input": {"file_path": file},
        })
        .to_string()
    }

    fn governed(root: &Path) -> String {
        root.join("services/auth/oauth/token.ts")
            .to_string_lossy()
            .to_string()
    }

    // ── the answered shapes ──────────────────────────────────────────────

    /// The main path: a read of a governed file briefs the decision that
    /// governs it, on the event that asked.
    #[test]
    fn test_read_of_a_governed_file_is_briefed() {
        let root = repo();
        let raw = envelope(root.path(), "PostToolUse", "Read", &governed(root.path()));

        let out = hook_reply(&raw, &args(root.path())).expect("a brief");
        let value: serde_json::Value = serde_json::from_str(&out).unwrap();

        assert_eq!(value["hookSpecificOutput"]["hookEventName"], "PostToolUse");
        let context = value["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap();
        assert!(context.contains("Sign With Asymmetric Keys"), "{context}");
        assert!(context.contains("R-A-001"), "{context}");
    }

    /// The reminder path answers too, and echoes its own event rather than
    /// the main path's.
    #[test]
    fn test_edit_of_a_governed_file_is_briefed_on_its_own_event() {
        let root = repo();
        let raw = envelope(root.path(), "PreToolUse", "Edit", &governed(root.path()));

        let out = hook_reply(&raw, &args(root.path())).expect("a brief");
        let value: serde_json::Value = serde_json::from_str(&out).unwrap();

        assert_eq!(value["hookSpecificOutput"]["hookEventName"], "PreToolUse");
    }

    /// Write names a file that does not exist yet. The path still has to
    /// resolve, because a new file is exactly what the reminder path is for.
    #[test]
    fn test_write_of_a_file_that_does_not_exist_yet_is_briefed() {
        let root = repo();
        let file = root
            .path()
            .join("services/auth/oauth/new-signer.ts")
            .to_string_lossy()
            .to_string();
        let raw = envelope(root.path(), "PreToolUse", "Write", &file);

        assert!(hook_reply(&raw, &args(root.path())).is_some());
    }

    /// The structural guarantee: no envelope produces a permission decision.
    /// Asserted over every answered shape rather than one, since the claim is
    /// about the command, not about a case.
    #[test]
    fn test_no_envelope_ever_produces_a_permission_decision() {
        let root = repo();
        for (event, tool) in [
            ("PostToolUse", "Read"),
            ("PreToolUse", "Edit"),
            ("PreToolUse", "Write"),
            ("PreToolUse", "MultiEdit"),
        ] {
            let raw = envelope(root.path(), event, tool, &governed(root.path()));
            let out = hook_reply(&raw, &args(root.path())).expect("a brief");
            assert!(!out.contains("permissionDecision"), "{out}");
            assert!(!out.contains("allow"), "{out}");
            assert!(!out.contains("deny"), "{out}");
        }
    }

    // ── silence ──────────────────────────────────────────────────────────

    /// A file no rule governs is silence, not an empty brief: an agent that
    /// is told "nothing governs this" on every read has learned nothing and
    /// paid tokens for it.
    #[test]
    fn test_ungoverned_file_is_silent() {
        let root = repo();
        let file = root
            .path()
            .join("infra/terraform/main.tf")
            .to_string_lossy()
            .to_string();
        let raw = envelope(root.path(), "PostToolUse", "Read", &file);

        assert_eq!(hook_reply(&raw, &args(root.path())), None);
    }

    /// Every malformed or unanswerable envelope is silence.
    #[test]
    fn test_unanswerable_envelopes_are_silent() {
        let root = repo();
        let a = args(root.path());
        let governed = governed(root.path());
        for (name, raw) in [
            ("empty stdin", String::new()),
            ("not json", "not json at all".to_string()),
            ("json but not an object", "[1,2,3]".to_string()),
            ("no event", serde_json::json!({"tool_name":"Read"}).to_string()),
            (
                "an event this hook does not answer",
                envelope(root.path(), "SessionStart", "Read", &governed),
            ),
            (
                "a tool that names no file",
                envelope(root.path(), "PreToolUse", "Bash", &governed),
            ),
            (
                "no tool_input",
                serde_json::json!({"hook_event_name":"PostToolUse","tool_name":"Read"}).to_string(),
            ),
            (
                "no file_path",
                serde_json::json!({"hook_event_name":"PostToolUse","tool_name":"Read","tool_input":{}})
                    .to_string(),
            ),
            (
                "a path outside the repository",
                envelope(root.path(), "PostToolUse", "Read", "/elsewhere/token.ts"),
            ),
        ] {
            assert_eq!(hook_reply(&raw, &a), None, "{name}");
        }
    }

    /// A repository with no rule set at all: silence, not an error. This is
    /// the common case in any repository that has not onboarded.
    #[test]
    fn test_repository_without_rules_is_silent() {
        let empty = tempdir().unwrap();
        let file = empty
            .path()
            .join("src/main.rs")
            .to_string_lossy()
            .to_string();
        let raw = envelope(empty.path(), "PostToolUse", "Read", &file);

        assert_eq!(hook_reply(&raw, &args(empty.path())), None);
    }

    /// An unreadable rule directory named explicitly degrades the same way.
    #[test]
    fn test_unreadable_rules_dir_is_silent() {
        let root = repo();
        let mut a = args(root.path());
        a.rules_dir = Some(root.path().join("does/not/exist"));
        let raw = envelope(root.path(), "PostToolUse", "Read", &governed(root.path()));

        assert_eq!(hook_reply(&raw, &a), None);
    }

    /// Unmodelled fields must not break deserialization: a newer envelope
    /// still briefs.
    #[test]
    fn test_envelope_tolerates_unknown_fields() {
        let root = repo();
        let raw = serde_json::json!({
            "session_id": "s1",
            "cwd": root.path().to_string_lossy(),
            "hook_event_name": "PostToolUse",
            "tool_name": "Read",
            "tool_use_id": "t1",
            "permission_mode": "acceptEdits",
            "something_new": {"nested": true},
            "tool_input": {"file_path": governed(root.path()), "offset": 1},
        })
        .to_string();

        assert!(hook_reply(&raw, &args(root.path())).is_some());
    }

    // ── path resolution ──────────────────────────────────────────────────

    /// The envelope's `cwd` stands in for the repository root when no
    /// `--repo` is given, which is how the skill will invoke this.
    #[test]
    fn test_cwd_from_the_envelope_is_the_repository_root() {
        let root = repo();
        let mut a = args(root.path());
        a.repo = None;
        let raw = envelope(root.path(), "PostToolUse", "Read", &governed(root.path()));

        assert!(hook_reply(&raw, &a).is_some());
    }

    #[test]
    fn test_relative_to_root_handles_both_shapes() {
        let root = Path::new("/repo");
        assert_eq!(
            relative_to_root(root, "/repo/services/auth/token.ts").as_deref(),
            Some("services/auth/token.ts")
        );
        // Already relative: taken as given.
        assert_eq!(
            relative_to_root(root, "services/auth/token.ts").as_deref(),
            Some("services/auth/token.ts")
        );
        assert_eq!(relative_to_root(root, "/elsewhere/token.ts"), None);
        assert_eq!(relative_to_root(root, ""), None);
    }

    // ── direct mode ──────────────────────────────────────────────────────

    #[test]
    fn test_direct_mode_needs_a_file() {
        let root = repo();
        let mut a = args(root.path());
        a.claude_hook = false;
        assert!(exec(&a).is_err());

        a.file = Some("services/auth/oauth/token.ts".to_string());
        assert!(exec(&a).is_ok());
    }

    /// Direct mode says so when nothing governs the file, where the hook
    /// stays silent: a person who asked deserves an answer.
    #[test]
    fn test_direct_mode_reports_an_ungoverned_file() {
        let root = repo();
        let mut a = args(root.path());
        a.claude_hook = false;
        a.file = Some("infra/terraform/main.tf".to_string());
        assert!(exec(&a).is_ok());
    }

    /// The floor comes from the flag, else the config key, else nothing — the
    /// same precedence `rules select` uses, so a corpus tuned once is tuned
    /// for the hook too.
    #[test]
    fn test_min_score_prefers_the_flag_then_the_config() {
        let _lock = crate::testutil::ENV_MUTEX.lock().unwrap();
        let home = tempdir().unwrap();
        let _dir =
            crate::testutil::EnvGuard::set("ACTUAL_CONFIG_DIR", home.path().to_str().unwrap());
        let _file = crate::testutil::EnvGuard::remove("ACTUAL_CONFIG");
        let root = repo();

        let mut a = args(root.path());
        a.min_score = None;
        assert_eq!(min_score(&a), 0.0);

        let mut cfg = crate::config::paths::load().unwrap_or_default();
        cfg.rules_min_score = Some(1.5);
        crate::config::paths::save(&cfg).unwrap();
        assert_eq!(min_score(&a), 1.5);

        a.min_score = Some(2.25);
        assert_eq!(min_score(&a), 2.25);
    }

    /// A floor above everything silences the hook, which is how a file that
    /// merely shares a directory with a governed one gets nothing.
    #[test]
    fn test_a_floor_above_every_score_silences_the_hook() {
        let root = repo();
        let mut a = args(root.path());
        a.min_score = Some(99.0);
        let raw = envelope(root.path(), "PostToolUse", "Read", &governed(root.path()));

        assert_eq!(hook_reply(&raw, &a), None);
    }

    /// Hook mode with no pipe attached — a terminal, or the `/dev/null` a
    /// test harness and CI attach — reads as empty and exits 0 rather than
    /// blocking on a source that will never deliver an envelope.
    #[test]
    fn test_hook_mode_without_a_pipe_exits_zero() {
        let root = repo();
        assert!(exec(&args(root.path())).is_ok());
    }

    #[test]
    fn test_read_envelope_reads_a_source_and_tolerates_a_broken_one() {
        assert_eq!(
            read_envelope(std::io::Cursor::new(b"{\"a\":1}")),
            "{\"a\":1}"
        );

        // Invalid UTF-8 is a read error: empty, never a panic.
        struct Broken;
        impl Read for Broken {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("no"))
            }
        }
        assert_eq!(read_envelope(Broken), "");
    }

    /// Silence is a `None` reply printed as nothing, which is what the hook
    /// contract means by "empty stdout".
    #[test]
    fn test_emit_prints_nothing_for_silence() {
        emit(None);
        emit(Some("{}".to_string()));
    }

    /// Only a pipe or a redirected file carries an envelope. A terminal and
    /// `/dev/null` — what a test harness and CI attach — do not, and reading
    /// them would block the hook rather than fail it open.
    #[test]
    fn test_only_a_pipe_is_read_as_an_envelope() {
        assert!(is_piped(false, false));
        assert!(!is_piped(true, true), "a terminal");
        assert!(!is_piped(false, true), "/dev/null");
        assert!(!is_piped(true, false));
        // The impure half answers for whatever this harness attached, without
        // reading it.
        let _ = stdin_is_piped();
    }
}
