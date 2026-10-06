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
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::cli::args::RulesBriefArgs;
use crate::error::ActualError;
use crate::rules::brief::{render_brief, BriefDecision};
use crate::rules::scope::{self, index::Query};

/// The event and tool pairs this command answers, and nothing else: a read is
/// briefed after it ran, an edit or write before it does. A brief after a
/// write arrives once the file is already changed, and a brief before a read
/// is one the read would have delivered anyway, so neither is answered even
/// if a hook matcher routes it here.
///
/// Only tools whose input carries an absolute `file_path` belong here. A tool
/// that names its file under another key (`NotebookEdit` uses
/// `notebook_path`) would always be silence, so listing it would overstate
/// what the hook can see.
const ANSWERED: &[(&str, &[&str])] = &[
    ("PostToolUse", &["Read"]),
    ("PreToolUse", &["Edit", "Write"]),
];

fn is_answered(event: &str, tool: &str) -> bool {
    ANSWERED
        .iter()
        .any(|(answered, tools)| *answered == event && tools.contains(&tool))
}

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
    read_envelope_if(stdin_is_piped(), std::io::stdin())
}

/// [`read_stdin`] over any source, so both sides of the guard are testable
/// whatever the test harness happens to attach to the real stdin.
fn read_envelope_if(piped: bool, source: impl Read) -> String {
    if !piped {
        return String::new();
    }
    read_envelope(source)
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

    // A `PostToolUse` on some other tool, a `PreToolUse` on a shell command,
    // or an event and tool that are not a documented pair name no file to
    // brief.
    let event = envelope.hook_event_name?;
    let tool = envelope.tool_name?;
    if !is_answered(&event, &tool) {
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
    let Some(relative) = relative_to_root(root, file) else {
        // Still empty stdout and exit 0, but a path that cannot be placed
        // under the root must not look the same as a file nothing governs.
        eprintln!(
            "actual rules brief: {file} is not under {}; no brief",
            root.display()
        );
        return None;
    };

    let rules_dir = args
        .rules_dir
        .clone()
        .unwrap_or_else(|| crate::rules::rules_dir(root));
    let resolved = scope::resolve_in(&rules_dir, root, false).ok()?;

    let query = Query::new("")
        .with_paths([relative.clone()])
        .with_min_score(min_score(args, root));
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

    render_brief(&relative, &briefed, args.rules_per_decision)
}

/// The score floor for this invocation, resolved exactly as `rules select`
/// resolves it: the flag, else this repository's floor, else the user-wide
/// config key, else none. An unreadable config degrades to no floor. A
/// manually edited config can bypass the validated `config set` path, so an
/// invalid value also degrades to no floor here — the hook fails open rather
/// than silently rejecting every document.
fn min_score(args: &RulesBriefArgs, root: &Path) -> f64 {
    super::rules_scope::effective_min_score(args.min_score, root).unwrap_or(0.0)
}

/// The path as the rule set names it: relative to the repository root, with
/// forward slashes.
///
/// A hook envelope carries absolute paths, while verify-block globs are
/// repository-relative, so an unconverted path matches nothing. Both sides go
/// through [`resolve`] first, so a symlinked checkout or a `/tmp` versus
/// `/private/tmp` spelling difference does not read as "outside the
/// repository". A path that really is outside yields `None` rather than a
/// guess — briefing a file in another checkout against these rules would be
/// wrong, not merely unhelpful.
fn relative_to_root(root: &Path, file: &str) -> Option<String> {
    let path = Path::new(file);
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    };
    let relative = resolve(&joined)
        .strip_prefix(resolve(root))
        .ok()?
        .to_path_buf();
    let text = relative
        .components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/");
    (!text.is_empty()).then_some(text)
}

/// `path` with symlinks, `.` and `..` resolved, whether or not it exists.
///
/// `canonicalize` alone fails on a file `Write` is about to create. So the
/// deepest ancestor that exists is canonicalized, and the rest is appended
/// with `.`/`..` collapsed lexically — which is exact there, since a path
/// that does not exist cannot contain a symlink.
fn resolve(path: &Path) -> PathBuf {
    for ancestor in path.ancestors() {
        let Ok(mut resolved) = ancestor.canonicalize() else {
            continue;
        };
        let rest = path.strip_prefix(ancestor).unwrap_or(Path::new(""));
        // `components` already drops an interior `.`, so only `..` needs
        // handling here.
        for component in rest.components() {
            if component == Component::ParentDir {
                resolved.pop();
            } else {
                resolved.push(component);
            }
        }
        return resolved;
    }
    path.to_path_buf()
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

    /// Event and tool are answered as pairs. A write is briefed before it
    /// runs, a read after; the other combinations are silence even though each
    /// half is answered on its own.
    #[test]
    fn test_only_documented_event_and_tool_pairs_are_answered() {
        let root = repo();
        let a = args(root.path());
        let file = governed(root.path());
        for (event, tool, answered) in [
            ("PostToolUse", "Read", true),
            ("PreToolUse", "Edit", true),
            ("PreToolUse", "Write", true),
            ("PostToolUse", "Write", false),
            ("PostToolUse", "Edit", false),
            ("PreToolUse", "Read", false),
            // Tools that do not carry an absolute `file_path`.
            ("PreToolUse", "MultiEdit", false),
            ("PreToolUse", "NotebookEdit", false),
        ] {
            let raw = envelope(root.path(), event, tool, &file);
            assert_eq!(hook_reply(&raw, &a).is_some(), answered, "{event}/{tool}");
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

    #[test]
    fn test_relative_to_root_collapses_dot_segments() {
        let root = Path::new("/repo");
        assert_eq!(
            relative_to_root(root, "/repo/a/../services/./token.ts").as_deref(),
            Some("services/token.ts")
        );
        assert_eq!(
            relative_to_root(root, "a/../services/token.ts").as_deref(),
            Some("services/token.ts")
        );
        // Escapes the root, absolute or relative.
        assert_eq!(relative_to_root(root, "/repo/../elsewhere/x.ts"), None);
        assert_eq!(relative_to_root(root, "../elsewhere/x.ts"), None);
    }

    /// A relative root that does not exist has no ancestor to canonicalize, so
    /// it is compared as written rather than failing.
    #[test]
    fn test_resolve_leaves_a_path_with_no_existing_ancestor_as_written() {
        let path = Path::new("no-such-dir-for-brief-test/a/../b.ts");
        assert_eq!(resolve(path), path);
        assert_eq!(
            relative_to_root(Path::new("no-such-dir-for-brief-test"), "a/b.ts").as_deref(),
            Some("a/b.ts")
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_relative_to_root_sees_through_a_symlinked_root() {
        let real = tempdir().unwrap();
        std::fs::create_dir_all(real.path().join("services/auth")).unwrap();
        std::fs::write(real.path().join("services/auth/token.ts"), "").unwrap();
        let aliases = tempdir().unwrap();
        let alias = aliases.path().join("checkout");
        std::os::unix::fs::symlink(real.path(), &alias).unwrap();

        // Root spelled one way, file the other, in both directions.
        for (root, file) in [
            (real.path(), alias.join("services/auth/token.ts")),
            (alias.as_path(), real.path().join("services/auth/token.ts")),
        ] {
            assert_eq!(
                relative_to_root(root, file.to_str().unwrap()).as_deref(),
                Some("services/auth/token.ts")
            );
        }
        // A `Write` of a file that does not exist yet, under a new directory.
        let new_file = alias.join("services/new/dir/token.ts");
        assert_eq!(
            relative_to_root(real.path(), new_file.to_str().unwrap()).as_deref(),
            Some("services/new/dir/token.ts")
        );
        // `..` through the symlink still lands outside.
        let escape = alias.join("../elsewhere.ts");
        assert_eq!(
            relative_to_root(real.path(), escape.to_str().unwrap()),
            None
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_hook_briefs_through_a_symlinked_root() {
        let root = repo();
        let aliases = tempdir().unwrap();
        let alias = aliases.path().join("checkout");
        std::os::unix::fs::symlink(root.path(), &alias).unwrap();
        let a = args(root.path());
        let raw = envelope(&alias, "PostToolUse", "Read", &governed(&alias));

        assert!(hook_reply(&raw, &a).is_some());
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
        assert_eq!(min_score(&a, root.path()), 0.0);

        let mut cfg = crate::config::paths::load().unwrap_or_default();
        cfg.rules_min_score = Some(1.5);
        crate::config::paths::save(&cfg).unwrap();
        assert_eq!(min_score(&a, root.path()), 1.5);

        a.min_score = Some(2.25);
        assert_eq!(min_score(&a, root.path()), 2.25);
    }

    /// The repository's own floor beats the user-wide one in the hook too, so
    /// a floor fitted to this rule set is the one the hook applies.
    #[test]
    fn test_min_score_prefers_the_repo_floor_over_the_user_wide_one() {
        let _lock = crate::testutil::ENV_MUTEX.lock().unwrap();
        let home = tempdir().unwrap();
        let _dir =
            crate::testutil::EnvGuard::set("ACTUAL_CONFIG_DIR", home.path().to_str().unwrap());
        let _file = crate::testutil::EnvGuard::remove("ACTUAL_CONFIG");
        let root = repo();
        let other = repo();
        let mut a = args(root.path());
        a.min_score = None;

        let mut cfg = crate::config::paths::load().unwrap_or_default();
        cfg.rules_min_score = Some(1.5);
        let key = crate::cli::commands::sync::compute_repo_key(root.path());
        crate::config::rules_floor::set_floor(&mut cfg, &key, 0.5).unwrap();
        crate::config::paths::save(&cfg).unwrap();

        assert_eq!(min_score(&a, root.path()), 0.5);
        assert_eq!(min_score(&a, other.path()), 1.5);
    }

    /// A manually edited config can bypass `config set` validation. The hook
    /// must fail open to no floor rather than let NaN reject every document.
    #[test]
    fn test_min_score_ignores_an_invalid_config_value() {
        let _lock = crate::testutil::ENV_MUTEX.lock().unwrap();
        let home = tempdir().unwrap();
        let _dir =
            crate::testutil::EnvGuard::set("ACTUAL_CONFIG_DIR", home.path().to_str().unwrap());
        let _file = crate::testutil::EnvGuard::remove("ACTUAL_CONFIG");
        let root = repo();
        let mut a = args(root.path());
        a.min_score = None;

        let mut cfg = crate::config::paths::load().unwrap_or_default();
        cfg.rules_min_score = Some(f64::NAN);
        crate::config::paths::save(&cfg).unwrap();

        assert_eq!(min_score(&a, root.path()), 0.0);
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
    fn test_read_envelope_if_reads_only_a_piped_source() {
        let envelope = b"{\"a\":1}";
        assert_eq!(
            read_envelope_if(true, std::io::Cursor::new(envelope)),
            "{\"a\":1}"
        );
        assert_eq!(read_envelope_if(false, std::io::Cursor::new(envelope)), "");
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
