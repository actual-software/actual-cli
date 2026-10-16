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
//! **Once per session.** A decision already briefed in this context is not
//! briefed again, and when every decision is already known the reply is
//! silence. The memory is [`super::brief_memory`]. Context compaction
//! empties the context the brief lived in, so `--claude-session-start` is the
//! entry point for a `SessionStart` hook: on `compact` (and `clear`) it
//! forgets what was briefed. Direct mode and an envelope without a
//! `session_id` have no session and brief every time.

use std::io::{IsTerminal, Read};
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::cli::args::RulesBriefArgs;
use crate::cli::commands::brief_ledger::{self, LedgerKey};
use crate::cli::commands::brief_memory::{self, SessionKey};
use crate::error::ActualError;
use crate::rules::brief::{render_brief_shown, BriefDecision};
use crate::rules::scope::{
    self,
    index::{AdrGroup, Match, Query, ScopeIndex},
};

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
    /// Set when the hook fires inside a subagent.
    pub agent_id: Option<String>,
    /// On `SessionStart`: `startup`, `resume`, `clear` or `compact`.
    pub source: Option<String>,
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
    if args.claude_session_start {
        session_start(&read_stdin(), args);
        return Ok(());
    }
    if args.claude_hook {
        emit(hook_reply(&read_stdin(), args));
        // Always `Ok`: see the fail-open note in the module docs.
        return Ok(());
    }
    exec_direct(args)
}

/// Answer a `SessionStart` envelope. Only a source that emptied the context
/// forgets anything: `startup` has no state yet, and `resume` restores the
/// transcript, brief included. The reset is for the agent whose context
/// emptied: a subagent's `agent_id` clears its memory, never the parent's.
/// Silent whatever happens.
fn session_start(raw: &str, args: &RulesBriefArgs) {
    let Ok(envelope) = serde_json::from_str::<HookEnvelope>(raw) else {
        return;
    };
    if !matches!(envelope.source.as_deref(), Some("compact" | "clear")) {
        return;
    }
    let Some(session_id) = envelope.session_id.filter(|id| !id.is_empty()) else {
        return;
    };
    let root = repo_root(args, || envelope_root(envelope.cwd));
    let rules_dir = args
        .rules_dir
        .clone()
        .unwrap_or_else(|| crate::rules::rules_dir(&root));
    if let Some(dir) = brief_memory::sessions_dir() {
        let agent_id = envelope.agent_id.as_deref().filter(|id| !id.is_empty());
        brief_memory::reset(&dir, &session_id, agent_id, &resolve(&rules_dir));
    }
}

/// Why no brief was produced.
///
/// Hook mode's stdout cannot say: it carries the brief the model reads, so any
/// explanation printed there would be read as governance. The reason goes to
/// stderr instead, which the shell hook keeps for `--debug` and a mod reads
/// directly.
///
/// [`Ungoverned`](Self::Ungoverned) and [`AlreadyBriefed`](Self::AlreadyBriefed)
/// are the pair that forced this. Both produce empty stdout, so a reader who
/// cannot separate them cannot tell a file no rule document covers from one
/// whose rules were stated earlier in the session. The mod reported both as a
/// single outcome, and its line claimed `package.json, pnpm-lock.yaml governed,
/// already briefed this session` for a lockfile that nothing governs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NoBrief {
    /// The path does not sit under the repository root.
    OutsideRoot,
    /// `resolve_in` failed: a rules path that is a file, or one that cannot
    /// be read. An absent directory is [`Ungoverned`](Self::Ungoverned) — it
    /// resolves to an empty index that searches and claims nothing.
    NoRules,
    /// Searched, and no rule document claims the file.
    Ungoverned,
    /// Rule documents claim it, and every one was briefed earlier.
    AlreadyBriefed,
    /// Claimed, and nothing renderable fit: no rule lines, none that
    /// `--max-chars` left room for, or a `--limit` of 0.
    NothingToShow,
}

impl NoBrief {
    /// The stable token a reader matches on. Prose above it may change.
    pub(crate) fn token(self) -> &'static str {
        match self {
            Self::OutsideRoot => "outside-root",
            Self::NoRules => "no-rules",
            Self::Ungoverned => "ungoverned",
            Self::AlreadyBriefed => "already-briefed",
            Self::NothingToShow => "nothing-to-show",
        }
    }

    /// The same fact as a sentence, for `--file` at a terminal.
    fn sentence(self, file: &str) -> String {
        match self {
            Self::OutsideRoot => format!("{file} is not under the repository root."),
            Self::NoRules => format!("No rule set to search for {file}."),
            Self::Ungoverned => format!("No rule document governs {file}."),
            Self::AlreadyBriefed => {
                format!("Every rule document governing {file} was briefed earlier this session.")
            }
            Self::NothingToShow => format!("No rule text fits the brief budget for {file}."),
        }
    }
}

/// The stderr line a reader matches on. Prose around it may change; this
/// shape must not.
fn report_line(reason: NoBrief, detail: &str) -> String {
    format!("actual rules brief: no-brief={} {detail}", reason.token())
}

/// Name the reason on stderr and stay silent on stdout.
///
/// Hook mode only. Direct mode prints [`NoBrief::sentence`] on stdout, and
/// printing both would report one silence twice.
fn report(reason: NoBrief, detail: &str) -> Option<String> {
    eprintln!("{}", report_line(reason, detail));
    None
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
    let root = repo_root(args, crate::cli::commands::sync::resolve_cwd);
    match brief_for(&root, &file, args, None) {
        Ok(brief) => println!("{brief}"),
        Err(reason) => println!("{}", reason.sentence(&file)),
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
pub(crate) fn read_stdin() -> String {
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
    let root = repo_root(args, || envelope_root(envelope.cwd));

    let session_id = envelope.session_id.filter(|id| !id.is_empty());
    let session = session_id.as_deref().map(|session_id| SessionIdentity {
        session_id,
        agent_id: envelope.agent_id.as_deref().filter(|id| !id.is_empty()),
    });
    let brief = match brief_for(&root, &file, args, session) {
        Ok(brief) => brief,
        Err(reason) => return report(reason, &relative_to_root(&root, &file).unwrap_or(file)),
    };
    serde_json::to_string(&ContextOutput {
        hook_specific_output: HookSpecificContext {
            hook_event_name: event,
            additional_context: brief,
        },
    })
    .ok()
}

/// Who is being briefed, for the once-per-session memory.
#[derive(Clone, Copy)]
struct SessionIdentity<'a> {
    session_id: &'a str,
    agent_id: Option<&'a str>,
}

/// What a decision is headed with: its title. A v2 decision keyed on an
/// `adr_id` whose title names no decision falls back to its best document's
/// title, since the id is not something to show an agent. A v1 decision falls
/// back to its key, which is its slug, as it always has.
fn brief_heading(decision: &AdrGroup) -> &str {
    decision
        .title
        .as_deref()
        .or_else(|| {
            decision
                .adr_id
                .as_ref()
                .and_then(|_| decision.documents.first()?.title.as_deref())
        })
        .unwrap_or(&decision.key)
}

/// The brief for one file, or the reason none was produced.
fn brief_for(
    root: &Path,
    file: &str,
    args: &RulesBriefArgs,
    session: Option<SessionIdentity<'_>>,
) -> Result<String, NoBrief> {
    let Some(relative) = relative_to_root(root, file) else {
        return Err(NoBrief::OutsideRoot);
    };

    let rules_dir = args
        .rules_dir
        .clone()
        .unwrap_or_else(|| crate::rules::rules_dir(root));
    let Ok(resolved) = scope::resolve_in(&rules_dir, root, false) else {
        return Err(NoBrief::NoRules);
    };

    // Uncapped, so whether a decision governs the file never depends on
    // `--limit`. The cap is applied below.
    let mut decisions = governing(&resolved.index, &relative, min_score(args, root));

    // Decisions this context was already briefed on are dropped after the
    // search, so `--limit` still means "the top N for this file" rather than
    // "N more than last time": the third-best decision does not surface just
    // because the first two were stated earlier.
    // The key is the resolved path: the read hook and the SessionStart hook
    // may be handed the same directory spelled differently (a symlink, a `..`),
    // and a reset that misses the file leaves a compacted agent unbriefed.
    let key_dir = resolve(&rules_dir);
    let memory = session.and_then(|who| {
        let dir = brief_memory::sessions_dir()?;
        let key = SessionKey {
            session_id: who.session_id,
            agent_id: who.agent_id,
            rules_dir: &key_dir,
        };
        let state = brief_memory::load(&dir, &key);
        Some((dir, key, state))
    });
    // Read before the cap and the filter: afterwards an empty list cannot say
    // whether the search found nothing, `--limit 0` left no room, or the
    // session had already stated all of it.
    let governed = !decisions.is_empty();
    if let Some(who) = session {
        record_folder(&resolved.index, who, &key_dir, &relative, governed);
    }
    decisions.truncate(args.limit);
    if let Some((_, _, state)) = &memory {
        decisions.retain(|decision| !state.has_briefed(&decision.key));
    }
    if decisions.is_empty() {
        return Err(match (governed, args.limit) {
            (false, _) => NoBrief::Ungoverned,
            (true, 0) => NoBrief::NothingToShow,
            (true, _) => NoBrief::AlreadyBriefed,
        });
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
            heading: brief_heading(decision),
            documents: parsed
                .iter()
                .filter(|(at, _)| *at == position)
                .map(|(_, document)| document)
                .collect(),
        })
        .collect();

    let Some((brief, shown)) =
        render_brief_shown(&relative, &briefed, args.rules_per_decision, args.max_chars)
    else {
        return Err(NoBrief::NothingToShow);
    };

    // The session-wide record counts what the memory below remembers, by the
    // same rule, but keys on the session alone: subagents add to one count,
    // and compaction never resets it.
    if let (Some(who), Some(dir)) = (session, brief_ledger::ledger_dir()) {
        let key = LedgerKey {
            session_id: who.session_id,
            rules_dir: &key_dir,
        };
        let added = shown
            .iter()
            .map(|&position| decisions[position].key.as_str());
        brief_ledger::record(&dir, &key, added);
    }

    // Only decisions that contributed everything they could are remembered:
    // shown in full, or trimmed to `--rules-per-decision`, which is a ceiling
    // no later read can lift. A decision `--max-chars` left out or trimmed
    // stays eligible, because a read with less competition can fit it whole.
    if let Some((dir, key, mut state)) = memory {
        for position in shown {
            state.record(&decisions[position].key);
        }
        brief_memory::store(&dir, &key, &state);
    }
    Ok(brief)
}

/// Note the folder `relative` sits in, in the session's record: covered when
/// the search found a decision, uncovered when it found none. A rule set with
/// no documents notes nothing, since every folder would read as uncovered.
fn record_folder(
    index: &ScopeIndex,
    who: SessionIdentity<'_>,
    rules_dir: &Path,
    relative: &str,
    governed: bool,
) {
    if index.is_empty() {
        return;
    }
    let (Some(folder), Some(dir)) = (
        brief_ledger::folder_of(relative),
        brief_ledger::ledger_dir(),
    ) else {
        return;
    };
    let key = LedgerKey {
        session_id: who.session_id,
        rules_dir,
    };
    brief_ledger::record_folder(&dir, &key, &folder, governed);
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

/// Every decision governing the repository-relative `relative`, in rank
/// order and uncapped. `plan-check` asks this of the paths a plan names, so a
/// planned path and a read one are judged alike.
///
/// A glob in a `verify` block is the evidence a rule governs a path, so the
/// path alone is searched first. Only when no rule claims it is the path
/// retried as query text, so the title and scope fields can match the terms
/// it spells. That keeps a rule set whose `verify` blocks name no paths from
/// being briefed on nothing, without letting a word match displace a glob
/// match under `--limit`. The retry also needs more than one shared word; see
/// [`MIN_RETRY_TERMS`].
///
/// The text is the path with `/` turned into spaces. The tokenizer splits on
/// both, so the terms are the same, but text with no slash names no path: the
/// query also reads paths out of its text, splitting on whitespace, and
/// `docs/My Notes/a.md` would add `Notes/a.md` as a second, wrong path. A
/// file's contents are not used: imports and boilerplate swamp the terms that
/// say what the file is about, and it would cost a read on every brief.
pub(crate) fn governing(index: &ScopeIndex, relative: &str, floor: f64) -> Vec<AdrGroup> {
    let by_path = Query::new("")
        .with_paths([relative.to_string()])
        .with_min_score(floor);
    let decisions = index.search_adrs(&by_path, index.len());
    if !decisions.is_empty() {
        return decisions;
    }
    let by_text = Query::new(relative.replace('/', " "))
        .with_paths([relative.to_string()])
        .with_min_score(floor);
    retry_by_text(index, &by_text)
}

/// Distinct words a document must share with the path before the text retry
/// briefs it on words alone. One shared word is too often a folder name every
/// codebase has — `test`, `config`, `utils` — to say a rule governs the file.
const MIN_RETRY_TERMS: usize = 2;

/// The text retry's decisions in rank order, uncapped, keeping only documents
/// with at least [`MIN_RETRY_TERMS`] matched words.
///
/// A glob match is no exemption. The retry only runs when the path search
/// found nothing at the floor, so a glob-matched document reaching it scored
/// below that floor; one shared word must not lift it back over.
///
/// The filter runs before the caller's cap, so a decision dropped here makes
/// room for the next one that qualifies rather than leaving the brief short.
fn retry_by_text(index: &ScopeIndex, query: &Query) -> Vec<AdrGroup> {
    let mut decisions: Vec<AdrGroup> = index
        .search_adrs(query, index.len())
        .into_iter()
        .filter_map(|mut decision| {
            decision.documents.retain(clears_retry_minimum);
            let best = decision.documents.first()?.score;
            decision.score = best;
            Some(decision)
        })
        .collect();
    // A decision whose best document was dropped now ranks on its next one.
    // The sort is stable, so ties keep the index's order.
    decisions.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    decisions
}

fn clears_retry_minimum(hit: &Match) -> bool {
    let words: std::collections::BTreeSet<&str> = hit
        .contributions
        .iter()
        .flat_map(|contribution| contribution.matched.iter().map(String::as_str))
        .collect();
    words.len() >= MIN_RETRY_TERMS
}

/// The repository root: `--repo`, else `<root>` when `--rules-dir` is
/// `<root>/.actual/rules`, else `fallback`.
///
/// A hook envelope's `cwd` follows the agent's `cd`. Taken as the root, it
/// places every path against whatever directory the shell last moved to, so a
/// conventional rules directory, which never moves, is preferred to it.
fn repo_root(args: &RulesBriefArgs, fallback: impl FnOnce() -> PathBuf) -> PathBuf {
    args.repo
        .clone()
        .or_else(|| args.rules_dir.as_deref().and_then(root_of_rules_dir))
        .unwrap_or_else(fallback)
}

fn root_of_rules_dir(rules_dir: &Path) -> Option<PathBuf> {
    if !rules_dir.ends_with(crate::rules::RULES_DIR_NAME) {
        return None;
    }
    let root = rules_dir.parent()?.parent()?;
    (!root.as_os_str().is_empty()).then(|| root.to_path_buf())
}

/// The envelope's `cwd`, else the process's own.
fn envelope_root(cwd: Option<String>) -> PathBuf {
    cwd.map(PathBuf::from)
        .unwrap_or_else(crate::cli::commands::sync::resolve_cwd)
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
pub(crate) fn relative_to_root(root: &Path, file: &str) -> Option<String> {
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
pub(crate) fn resolve(path: &Path) -> PathBuf {
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

    /// A rule document whose `Verify` block names no path at all, only a
    /// generic command. Whole rule sets are written this way -- one measured
    /// corpus had 1 of 155 documents naming any path -- and against them the
    /// glob signal matches nothing, so the query text is the only signal left.
    const PATHLESS: &str = "# Reading Progress Module Adoption\n\nThese rules are ALWAYS ACTIVE for reading progress components.\n\n### Rules\n\n- **R-P-001** MUST: read progress through the module.\n\n### Verify\n\n```bash\nnpm run test\n```\n";

    /// Names `expiry`, which the OAuth rule does not, so a file called
    /// `expiry.ts` under `services/auth/oauth/` matches it on words alone.
    /// Names no path.
    const TOKEN_WORDS: &str = "# Token Expiry Policy\n\nThese rules are ALWAYS ACTIVE for OAuth token expiry in auth services.\n\n### Rules\n\n- **R-T-001** MUST: expire tokens within an hour.\n\n### Verify\n\n```bash\nnpm run test\n```\n";

    /// Shares `bar` and `component` with `reading-progress/ProgressBar.tsx`:
    /// enough words to clear the retry minimum, fewer and commoner than the
    /// reading-progress rule's.
    const CHARTS: &str = "# Bar Chart Rendering\n\nThese rules are ALWAYS ACTIVE for bar chart components.\n\n### Rules\n\n- **R-C-001** MUST: render charts with the shared axis.\n\n### Verify\n\n```bash\nnpm run lint\n```\n";

    /// Shares only `app` with `app/components/reading-progress/ProgressBar.tsx`.
    const LOGGING: &str = "# Request Logging\n\nThese rules are ALWAYS ACTIVE for logging in app services.\n\n### Rules\n\n- **R-L-001** MUST: log every request with its trace id.\n\n### Verify\n\n```bash\nnpm run lint\n```\n";

    /// Governs `Notes/**` at the repository root.
    const RELEASE_NOTES: &str = "# Release Checklist\n\nThese rules are ALWAYS ACTIVE for release checklists.\n\n### Rules\n\n- **R-N-001** MUST: list every migration.\n\n### Verify\n\n```bash\ngrep -r \"Migration\" Notes/ --include=\"*.md\"\n```\n";

    /// Several pathless documents, so terms are weighted by inverse document
    /// frequency. A one-document rule set takes the `term_weight` fallback
    /// instead, where every known word counts in full and nothing is ranked.
    fn pathless_repo() -> TempDir {
        let root = tempdir().unwrap();
        let dir = crate::rules::rules_dir(root.path());
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("app-reading-progress-7f21.md"), PATHLESS).unwrap();
        std::fs::write(dir.join("bar-chart-rendering-3c4d.md"), CHARTS).unwrap();
        std::fs::write(dir.join("request-logging-9e1b.md"), LOGGING).unwrap();
        root
    }

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
            claude_session_start: false,
            file: None,
            repo: Some(root.to_path_buf()),
            rules_dir: None,
            limit: 2,
            rules_per_decision: 8,
            max_chars: crate::rules::brief::DEFAULT_MAX_CHARS,
            // Explicit, so these tests never read the machine's own config.
            min_score: Some(0.0),
        }
    }

    fn envelope(root: &Path, event: &str, tool: &str, file: &str) -> String {
        serde_json::json!({
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

    // ── v2 root-relative scope and excludes ──────────────────────────────

    /// A v2 decision rooted at `packages/web`, excluding test files. Its title
    /// shares the words `src` and `test` with the paths below, so only the
    /// exclusion — not a lack of word overlap — keeps the test file unbriefed.
    const WEB_V2: &str = "---\nversion: 2\nname: web-src-test-1a2b\nmetadata:\n  actual:\n    adr_id: adr-web\n  scope:\n    root: packages/web\n    paths:\n      - \"src/**\"\n      - \"!src/**/*.test.ts\"\n---\n\n# Web src test conventions: Components\n\nScope: applies to `src/**`; excludes `!src/**/*.test.ts`.\n\n### Rules\n\n- **R-WEB-001** MUST: Components stay presentational.\n";

    #[test]
    fn test_a_rooted_adr_briefs_inside_its_root_and_not_for_excluded_or_outside_files() {
        with_scratch_config(|| {
            let root = tempdir().unwrap();
            let dir = crate::rules::rules_dir(root.path());
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("web-src-test-1a2b.md"), WEB_V2).unwrap();
            let a = args(root.path());
            let path = |file: &str| root.path().join(file).to_string_lossy().to_string();

            assert!(brief_for(root.path(), &path("packages/web/src/x.ts"), &a, None).is_ok());
            assert_eq!(
                brief_for(root.path(), &path("packages/web/src/x.test.ts"), &a, None),
                Err(NoBrief::Ungoverned)
            );
            assert_eq!(
                brief_for(root.path(), &path("packages/api/src/x.ts"), &a, None),
                Err(NoBrief::Ungoverned)
            );
        });
    }

    /// A v2 decision whose title names no decision, so the heading must not be
    /// the `adr_id` its key now is.
    fn titled_v2(adr_id: &str) -> String {
        format!(
            "---\nversion: 2\nname: pinning\nmetadata:\n  actual:\n    adr_id: {adr_id}\n  scope:\n    root: .\n    paths:\n      - \"infra/**\"\n---\n\n# Pin Terraform Providers\n\n### Rules\n\n- **R-TF-001** MUST: pin every provider.\n"
        )
    }

    #[test]
    fn test_a_v2_brief_is_headed_by_its_title_not_its_adr_id() {
        with_scratch_config(|| {
            let root = tempdir().unwrap();
            let dir = crate::rules::rules_dir(root.path());
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("pinning-1a2b.md"), titled_v2("3f9c-uuid")).unwrap();
            let a = args(root.path());
            let file = root
                .path()
                .join("infra/main.tf")
                .to_string_lossy()
                .to_string();

            let brief = brief_for(root.path(), &file, &a, None).unwrap();
            assert!(brief.contains("## Pin Terraform Providers"), "{brief}");
            assert!(!brief.contains("3f9c-uuid"), "{brief}");
        });
    }

    #[test]
    fn test_renaming_a_v2_file_does_not_rebrief_within_a_session() {
        with_scratch_config(|| {
            let root = tempdir().unwrap();
            let dir = crate::rules::rules_dir(root.path());
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("pinning-1a2b.md"), titled_v2("3f9c-uuid")).unwrap();
            let a = args(root.path());
            let file = root
                .path()
                .join("infra/main.tf")
                .to_string_lossy()
                .to_string();
            let who = SessionIdentity {
                session_id: "s1",
                agent_id: None,
            };
            assert!(brief_for(root.path(), &file, &a, Some(who)).is_ok());

            std::fs::rename(dir.join("pinning-1a2b.md"), dir.join("renamed-9z9z.md")).unwrap();
            assert_eq!(
                brief_for(root.path(), &file, &a, Some(who)),
                Err(NoBrief::AlreadyBriefed)
            );
        });
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

    /// A rule set with no paths in its `Verify` blocks still briefs: when no
    /// glob claims the file, the path is retried as query text and the title
    /// and scope fields match on the terms a filename spells.
    #[test]
    fn test_a_rule_set_without_paths_is_still_briefed() {
        let root = pathless_repo();
        let file = root
            .path()
            .join("app/components/reading-progress/ProgressBar.tsx")
            .to_string_lossy()
            .to_string();
        let raw = envelope(root.path(), "PostToolUse", "Read", &file);
        let args = RulesBriefArgs {
            limit: 3,
            ..args(root.path())
        };

        let reply = hook_reply(&raw, &args).expect("a brief");
        let progress = reply
            .find("R-P-001")
            .unwrap_or_else(|| panic!("the reading-progress rule should be briefed: {reply}"));
        let charts = reply
            .find("R-C-001")
            .unwrap_or_else(|| panic!("a two-word match should be briefed: {reply}"));
        assert!(
            progress < charts,
            "the rule sharing more and rarer words should rank first: {reply}"
        );
        assert!(
            !reply.contains("R-L-001"),
            "a one-word match should not be briefed, even with room under --limit: {reply}"
        );
    }

    /// The text signal is not a wildcard: a file whose path shares no term
    /// with the rule set is still silent. Otherwise every read in a repository
    /// would brief everything.
    #[test]
    fn test_path_text_does_not_match_an_unrelated_file() {
        let root = pathless_repo();
        let file = root
            .path()
            .join("infra/terraform/main.tf")
            .to_string_lossy()
            .to_string();
        let raw = envelope(root.path(), "PostToolUse", "Read", &file);

        assert_eq!(hook_reply(&raw, &args(root.path())), None);
    }

    /// One shared word is not enough for the text retry: `progress` alone in
    /// `lib/progress.ts` does not make the reading-progress rule govern it.
    #[test]
    fn test_a_single_shared_word_is_not_briefed() {
        let root = pathless_repo();
        let file = root
            .path()
            .join("lib/progress.ts")
            .to_string_lossy()
            .to_string();
        let raw = envelope(root.path(), "PostToolUse", "Read", &file);

        assert_eq!(hook_reply(&raw, &args(root.path())), None);
    }

    /// A path containing a space is one path. Read as prose, `docs/My
    /// Notes/a.md` would also name `Notes/a.md`, which a rule globbing
    /// `Notes/**` covers, though the file is not under `Notes/`.
    #[test]
    fn test_a_path_with_a_space_is_not_split_into_another_path() {
        let root = tempdir().unwrap();
        let dir = crate::rules::rules_dir(root.path());
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("release-checklist-5d2a.md"), RELEASE_NOTES).unwrap();
        let file = root
            .path()
            .join("docs/My Notes/a.md")
            .to_string_lossy()
            .to_string();
        let raw = envelope(root.path(), "PostToolUse", "Read", &file);

        assert_eq!(hook_reply(&raw, &args(root.path())), None);
    }

    /// The text retry is a fallback, not a second signal: when a glob claims
    /// the file, a rule that only shares words with its path is not briefed,
    /// even with room under `--limit` for both.
    #[test]
    fn test_a_glob_match_keeps_word_only_matches_out() {
        let root = repo();
        let dir = crate::rules::rules_dir(root.path());
        std::fs::write(dir.join("cross-cutting-token-expiry-b3c9.md"), TOKEN_WORDS).unwrap();
        let file = root
            .path()
            .join("services/auth/oauth/expiry.ts")
            .to_string_lossy()
            .to_string();
        let raw = envelope(root.path(), "PostToolUse", "Read", &file);

        let reply = hook_reply(&raw, &args(root.path())).expect("a brief");
        assert!(
            reply.contains("R-A-001"),
            "the glob match should be briefed: {reply}"
        );
        assert!(
            !reply.contains("R-T-001"),
            "a word-only match should not ride along with a glob match: {reply}"
        );
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
    /// still briefs. It names a session, so it runs against scratch state
    /// rather than leaving a file in the real config directory.
    #[test]
    fn test_envelope_tolerates_unknown_fields() {
        with_scratch_config(|| {
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
        });
    }

    // ── once per session ─────────────────────────────────────────────────

    /// A hook envelope that belongs to a session.
    fn session_envelope(root: &Path, session: &str, agent: Option<&str>, file: &str) -> String {
        let mut value: serde_json::Value =
            serde_json::from_str(&envelope(root, "PostToolUse", "Read", file)).unwrap();
        value["session_id"] = session.into();
        if let Some(agent) = agent {
            value["agent_id"] = agent.into();
        }
        value.to_string()
    }

    fn session_start_envelope(root: &Path, session: &str, source: &str) -> String {
        serde_json::json!({
            "session_id": session,
            "cwd": root.to_string_lossy(),
            "hook_event_name": "SessionStart",
            "source": source,
        })
        .to_string()
    }

    /// Run `body` with the config directory pointed at a scratch one, so no
    /// test writes to the machine's real session state.
    fn with_scratch_config(body: impl FnOnce()) {
        let _lock = crate::testutil::ENV_MUTEX.lock().unwrap();
        let home = tempdir().unwrap();
        let _dir =
            crate::testutil::EnvGuard::set("ACTUAL_CONFIG_DIR", home.path().to_str().unwrap());
        let _file = crate::testutil::EnvGuard::remove("ACTUAL_CONFIG");
        body();
    }

    /// The decision is stated once; asking again in the same session is
    /// silence, and a different session is briefed afresh.
    #[test]
    fn test_a_decision_is_briefed_once_per_session() {
        with_scratch_config(|| {
            let root = repo();
            let a = args(root.path());
            let file = governed(root.path());

            let first = session_envelope(root.path(), "s1", None, &file);
            assert!(hook_reply(&first, &a).is_some());
            assert_eq!(hook_reply(&first, &a), None);

            let other = session_envelope(root.path(), "s2", None, &file);
            assert!(hook_reply(&other, &a).is_some());
        });
    }

    /// A decision with more MUST rules than `--rules-per-decision` is briefed
    /// once too. The cap is everything it can ever contribute, so a second
    /// read would repeat the identical truncated block without adding a rule.
    #[test]
    fn test_a_decision_over_the_per_decision_cap_is_briefed_once() {
        with_scratch_config(|| {
            let root = repo();
            std::fs::write(
                crate::rules::rules_dir(root.path()).join("cross-cutting-token-signing-e410.md"),
                OAUTH.replace(
                    "- **R-A-002** SHOULD: rotate keys quarterly.",
                    "- **R-A-002** MUST NOT: sign with HS256.",
                ),
            )
            .unwrap();
            let mut a = args(root.path());
            a.rules_per_decision = 1;
            let read = session_envelope(root.path(), "s1", None, &governed(root.path()));

            let first = hook_reply(&read, &a).expect("a brief");
            assert!(first.contains("(1 of 2 rules shown)"), "{first}");
            assert_eq!(hook_reply(&read, &a), None);
        });
    }

    /// A second file governed by the same decision adds nothing new.
    #[test]
    fn test_another_file_of_the_same_decision_is_silent() {
        with_scratch_config(|| {
            let root = repo();
            let a = args(root.path());
            let one = session_envelope(root.path(), "s1", None, &governed(root.path()));
            let two = session_envelope(
                root.path(),
                "s1",
                None,
                &root
                    .path()
                    .join("services/auth/oauth/other.ts")
                    .to_string_lossy(),
            );

            assert!(hook_reply(&one, &a).is_some());
            assert_eq!(hook_reply(&two, &a), None);
        });
    }

    /// `--limit` is the top N for the file, applied before the already-briefed
    /// are dropped: once those N are stated, the read is silent even though a
    /// lower-ranked decision exists, and that one does not surface in its
    /// place. A fresh session gets the same top N, so silence never hides what
    /// a stateless brief would have said.
    #[test]
    fn test_limit_applies_before_briefed_decisions_are_dropped() {
        with_scratch_config(|| {
            let root = repo();
            let other = OAUTH
                .replace(
                    "Sign With Asymmetric Keys: Token Signing",
                    "Rotate Keys: Rotation",
                )
                .replace("R-A-", "R-B-");
            std::fs::write(
                crate::rules::rules_dir(root.path()).join("cross-cutting-rotation-b7f2.md"),
                other,
            )
            .unwrap();
            let mut a = args(root.path());
            a.limit = 1;
            let file = governed(root.path());

            let first = hook_reply(&session_envelope(root.path(), "s1", None, &file), &a)
                .expect("the top decision is briefed");
            assert_eq!(
                first.matches("## ").count(),
                1,
                "limit 1 briefs one decision: {first}"
            );

            assert_eq!(
                hook_reply(&session_envelope(root.path(), "s1", None, &file), &a),
                None,
                "the runner-up must not be promoted once the top decision is stated"
            );

            let fresh = hook_reply(&session_envelope(root.path(), "s2", None, &file), &a);
            assert_eq!(fresh, Some(first));
        });
    }

    /// After compaction the brief is gone from context, so the session-start
    /// reset makes the decision eligible again.
    #[test]
    fn test_reset_on_compact_briefs_again() {
        with_scratch_config(|| {
            let root = repo();
            let a = args(root.path());
            let read = session_envelope(root.path(), "s1", None, &governed(root.path()));
            assert!(hook_reply(&read, &a).is_some());
            assert_eq!(hook_reply(&read, &a), None);

            session_start(&session_start_envelope(root.path(), "s1", "compact"), &a);

            assert!(hook_reply(&read, &a).is_some());
        });
    }

    /// A subagent's context is its own: its compaction makes its decisions
    /// eligible again without touching the parent's, and the parent's
    /// compaction leaves the subagent's alone.
    #[test]
    fn test_reset_targets_only_the_agent_whose_context_emptied() {
        with_scratch_config(|| {
            let root = repo();
            let a = args(root.path());
            let file = governed(root.path());
            let parent = session_envelope(root.path(), "s1", None, &file);
            let child = session_envelope(root.path(), "s1", Some("sub1"), &file);
            assert!(hook_reply(&parent, &a).is_some());
            assert!(hook_reply(&child, &a).is_some());

            let mut start: serde_json::Value =
                serde_json::from_str(&session_start_envelope(root.path(), "s1", "compact"))
                    .unwrap();
            start["agent_id"] = "sub1".into();
            session_start(&start.to_string(), &a);

            assert!(hook_reply(&child, &a).is_some(), "subagent forgot");
            assert_eq!(hook_reply(&parent, &a), None, "parent was cleared");

            session_start(&session_start_envelope(root.path(), "s1", "compact"), &a);

            assert!(hook_reply(&parent, &a).is_some(), "parent forgot");
            assert_eq!(hook_reply(&child, &a), None, "subagent was cleared");
        });
    }

    /// The read hook and the SessionStart hook can name the same rules
    /// directory differently; the reset must still find the file the read wrote.
    #[test]
    fn test_reset_finds_state_under_a_differently_spelled_rules_dir() {
        with_scratch_config(|| {
            let root = repo();
            let a = args(root.path());
            let read = session_envelope(root.path(), "s1", None, &governed(root.path()));
            assert!(hook_reply(&read, &a).is_some());
            assert_eq!(hook_reply(&read, &a), None);

            let mut spelled = args(root.path());
            spelled.rules_dir = Some(
                crate::rules::rules_dir(root.path())
                    .join("..")
                    .join(crate::rules::rules_dir(root.path()).file_name().unwrap()),
            );
            session_start(
                &session_start_envelope(root.path(), "s1", "compact"),
                &spelled,
            );

            assert!(hook_reply(&read, &a).is_some(), "reset missed the file");
        });
    }

    /// `exec` routes `--claude-session-start` to the reset, reading the
    /// envelope from stdin, which is empty under a test harness: nothing to
    /// forget, exit 0.
    #[test]
    fn test_exec_session_start_mode_exits_zero() {
        let root = repo();
        let mut a = args(root.path());
        a.claude_hook = false;
        a.claude_session_start = true;
        assert!(exec(&a).is_ok());
    }

    /// A reset envelope with no session cannot say whose memory to clear, so
    /// it clears none.
    #[test]
    fn test_reset_without_a_session_id_forgets_nothing() {
        with_scratch_config(|| {
            let root = repo();
            let a = args(root.path());
            let read = session_envelope(root.path(), "s1", None, &governed(root.path()));
            assert!(hook_reply(&read, &a).is_some());

            for start in [
                serde_json::json!({"cwd": root.path().to_string_lossy(), "source": "compact"}),
                serde_json::json!({"session_id": "", "source": "clear"}),
            ] {
                session_start(&start.to_string(), &a);
            }

            assert_eq!(hook_reply(&read, &a), None);
        });
    }

    /// Without a usable config directory there is nowhere to keep state: the
    /// reset does nothing and every read is briefed, never silenced.
    #[test]
    fn test_an_unusable_config_dir_means_no_memory() {
        let _lock = crate::testutil::ENV_MUTEX.lock().unwrap();
        let _dir = crate::testutil::EnvGuard::set("ACTUAL_CONFIG_DIR", "not/absolute");
        let _file = crate::testutil::EnvGuard::remove("ACTUAL_CONFIG");
        let root = repo();
        let a = args(root.path());
        let read = session_envelope(root.path(), "s1", None, &governed(root.path()));

        assert!(hook_reply(&read, &a).is_some());
        assert!(hook_reply(&read, &a).is_some());
        session_start(&session_start_envelope(root.path(), "s1", "compact"), &a);
    }

    /// `startup` and `resume` keep the context the brief lives in, so they
    /// must not forget it.
    #[test]
    fn test_reset_ignores_sources_that_keep_the_context() {
        with_scratch_config(|| {
            let root = repo();
            let a = args(root.path());
            let read = session_envelope(root.path(), "s1", None, &governed(root.path()));
            assert!(hook_reply(&read, &a).is_some());

            for source in ["startup", "resume"] {
                session_start(&session_start_envelope(root.path(), "s1", source), &a);
            }
            session_start("not json", &a);

            assert_eq!(hook_reply(&read, &a), None);
        });
    }

    /// A subagent's context never saw the parent's brief, so it is briefed
    /// even where the envelope hands it the parent's `session_id`.
    #[test]
    fn test_a_subagent_is_briefed_independently_of_its_parent() {
        with_scratch_config(|| {
            let root = repo();
            let a = args(root.path());
            let file = governed(root.path());

            let parent = session_envelope(root.path(), "s1", None, &file);
            let child = session_envelope(root.path(), "s1", Some("agent-7"), &file);
            assert!(hook_reply(&parent, &a).is_some());
            assert!(hook_reply(&child, &a).is_some());
            assert_eq!(hook_reply(&child, &a), None);
        });
    }

    /// Fail open: damaged state means briefing again, never a hook error.
    #[test]
    fn test_a_corrupt_state_file_briefs_again() {
        with_scratch_config(|| {
            let root = repo();
            let a = args(root.path());
            let read = session_envelope(root.path(), "s1", None, &governed(root.path()));
            assert!(hook_reply(&read, &a).is_some());

            let dir = brief_memory::sessions_dir().unwrap();
            for entry in std::fs::read_dir(&dir).unwrap().flatten() {
                std::fs::write(entry.path(), "{ not json").unwrap();
            }

            assert!(hook_reply(&read, &a).is_some());
        });
    }

    /// Without a session there is nothing to remember, so every read briefs.
    #[test]
    fn test_an_envelope_without_a_session_briefs_every_time() {
        with_scratch_config(|| {
            let root = repo();
            let a = args(root.path());
            let raw = envelope(root.path(), "PostToolUse", "Read", &governed(root.path()));

            assert!(hook_reply(&raw, &a).is_some());
            assert!(hook_reply(&raw, &a).is_some());
        });
    }

    // ── the session-wide record ──────────────────────────────────────────

    /// A second decision, governing files [`OAUTH`] does not.
    const TERRAFORM: &str = "# Pin Providers: Provider Versions\n\nThese rules are ALWAYS ACTIVE for Terraform in `infra/terraform/`.\n\n### Rules\n\n- **R-P-001** MUST: pin every provider to an exact version.\n\n### Verify\n\n```bash\ngrep -r \"version\" infra/terraform/ --include=\"*.tf\"\n```\n";

    /// The decisions in `session`'s record for the rules under `root`.
    fn ledger(root: &Path, session: &str) -> Vec<String> {
        let rules_dir = resolve(&crate::rules::rules_dir(root));
        let key = LedgerKey {
            session_id: session,
            rules_dir: &rules_dir,
        };
        brief_ledger::load(&brief_ledger::ledger_dir().unwrap(), &key)
            .briefed
            .iter()
            .cloned()
            .collect()
    }

    /// The main agent and its subagents add to one record, and a decision
    /// briefed to both counts once.
    #[test]
    fn test_the_session_record_adds_up_the_main_agent_and_subagents() {
        with_scratch_config(|| {
            let root = repo();
            std::fs::write(
                crate::rules::rules_dir(root.path())
                    .join("cross-cutting-provider-versions-c3d1.md"),
                TERRAFORM,
            )
            .unwrap();
            let a = args(root.path());
            let signing = governed(root.path());
            let terraform = root
                .path()
                .join("infra/terraform/main.tf")
                .to_string_lossy()
                .to_string();

            assert!(hook_reply(&session_envelope(root.path(), "s1", None, &signing), &a).is_some());
            assert_eq!(ledger(root.path(), "s1"), ["Sign With Asymmetric Keys"]);

            for file in [&terraform, &signing] {
                let read = session_envelope(root.path(), "s1", Some("sub-1"), file);
                assert!(hook_reply(&read, &a).is_some(), "{file}");
            }

            assert_eq!(
                ledger(root.path(), "s1"),
                ["Pin Providers", "Sign With Asymmetric Keys"]
            );
            assert!(ledger(root.path(), "s2").is_empty());
        });
    }

    /// The compaction reset wipes the memory, so the decision is briefed
    /// again, and leaves the session record alone, so it still counts once.
    #[test]
    fn test_a_compaction_reset_leaves_the_session_record_alone() {
        with_scratch_config(|| {
            let root = repo();
            let a = args(root.path());
            let read = session_envelope(root.path(), "s1", None, &governed(root.path()));
            assert!(hook_reply(&read, &a).is_some());

            session_start(&session_start_envelope(root.path(), "s1", "compact"), &a);
            assert_eq!(ledger(root.path(), "s1"), ["Sign With Asymmetric Keys"]);

            assert!(hook_reply(&read, &a).is_some(), "the memory was not reset");
            assert_eq!(ledger(root.path(), "s1"), ["Sign With Asymmetric Keys"]);
        });
    }

    // ── the session's folders ────────────────────────────────────────────

    /// Names one file of `billing/` in its `Verify` block and shares only
    /// `billing` with the folder's other files, so they are governed by none.
    const INVOICES: &str = "# Invoice Totals\n\nThese rules are ALWAYS ACTIVE for invoice totals.\n\n### Rules\n\n- **R-I-001** MUST: round totals once.\n\n### Verify\n\n```bash\ngrep -n \"round(\" billing/invoice.rs\n```\n";

    /// The covered folders and the gaps in `session`'s record for the rules
    /// under `root`.
    fn folders(root: &Path, session: &str) -> (Vec<String>, Vec<String>) {
        let rules_dir = resolve(&crate::rules::rules_dir(root));
        let key = LedgerKey {
            session_id: session,
            rules_dir: &rules_dir,
        };
        let record = brief_ledger::load(&brief_ledger::ledger_dir().unwrap(), &key);
        let gaps = record
            .uncovered_folders
            .difference(&record.covered_folders)
            .cloned()
            .collect();
        (record.covered_folders.into_iter().collect(), gaps)
    }

    fn read_in(root: &Path, session: &str, file: &str) -> String {
        session_envelope(root, session, None, &root.join(file).to_string_lossy())
    }

    /// A governed file covers its folder and an ungoverned one leaves its
    /// folder a gap, both named from the repository root. A folder whose
    /// decision was briefed earlier in the session is covered all the same.
    #[test]
    fn test_a_read_notes_its_folder_as_covered_or_a_gap() {
        with_scratch_config(|| {
            let root = repo();
            let a = args(root.path());
            let reply = |file: &str| hook_reply(&read_in(root.path(), "s1", file), &a);

            assert!(reply("services/auth/oauth/token.ts").is_some());
            assert_eq!(reply("services/auth/oauth/keys/rotate.ts"), None);
            assert_eq!(reply("infra/terraform/main.tf"), None);

            assert_eq!(
                folders(root.path(), "s1"),
                (
                    vec![
                        "services/auth/oauth".to_string(),
                        "services/auth/oauth/keys".to_string()
                    ],
                    vec!["infra/terraform".to_string()]
                )
            );
        });
    }

    /// A folder holding one governed file and one ungoverned file is covered,
    /// whichever of the two is read first.
    #[test]
    fn test_a_mixed_folder_is_covered_in_either_order() {
        with_scratch_config(|| {
            let root = tempdir().unwrap();
            let dir = crate::rules::rules_dir(root.path());
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("invoice-totals-4b1e.md"), INVOICES).unwrap();
            let a = args(root.path());
            let (invoice, ledger) = ("billing/invoice.rs", "billing/ledger.py");
            let path = |file: &str| root.path().join(file).to_string_lossy().to_string();
            assert!(brief_for(root.path(), &path(invoice), &a, None).is_ok());
            assert_eq!(
                brief_for(root.path(), &path(ledger), &a, None),
                Err(NoBrief::Ungoverned)
            );

            for (session, order) in [("s1", [invoice, ledger]), ("s2", [ledger, invoice])] {
                for file in order {
                    hook_reply(&read_in(root.path(), session, file), &a);
                }
                assert_eq!(
                    folders(root.path(), session),
                    (vec!["billing".to_string()], vec![]),
                    "{session}"
                );
            }
        });
    }

    /// Docs, config, lockfiles and generated output note no folder, even
    /// where a decision governs them and the brief answers.
    #[test]
    fn test_files_that_never_count_note_no_folder() {
        with_scratch_config(|| {
            let root = repo();
            let a = args(root.path());
            let governed_config = read_in(root.path(), "s1", "services/auth/oauth/keys.json");
            assert!(hook_reply(&governed_config, &a).is_some());
            for file in [
                "infra/terraform/README.md",
                "Cargo.lock",
                "dist/oauth/token.js",
            ] {
                hook_reply(&read_in(root.path(), "s1", file), &a);
            }

            assert_eq!(folders(root.path(), "s1"), (vec![], vec![]));
        });
    }

    /// With no rule documents to apply, or a rule set that will not build,
    /// every folder would read as a gap, so none is noted.
    #[test]
    fn test_no_folder_is_noted_without_a_rule_set_to_apply() {
        with_scratch_config(|| {
            let bare = tempdir().unwrap();
            let read = read_in(bare.path(), "s1", "src/a.ts");
            assert_eq!(hook_reply(&read, &args(bare.path())), None);

            let not_a_dir = bare.path().join("rules-file");
            std::fs::write(&not_a_dir, "not a directory").unwrap();
            let mut unbuildable = args(bare.path());
            unbuildable.rules_dir = Some(not_a_dir);
            assert_eq!(hook_reply(&read, &unbuildable), None);

            let records = std::fs::read_dir(brief_ledger::ledger_dir().unwrap())
                .map(|entries| entries.count())
                .unwrap_or(0);
            assert_eq!(records, 0);
        });
    }

    // ── why no brief ─────────────────────────────────────────────────────

    /// The pair that forced the reason onto stderr. Both end in empty stdout,
    /// and a reader that cannot separate them calls an ungoverned file
    /// governed -- which a mod's own output did, for a lockfile.
    #[test]
    fn test_a_file_nothing_governs_and_one_already_briefed_differ() {
        with_scratch_config(|| {
            let root = repo();
            let a = args(root.path());

            let ungoverned = root.path().join("README.md").to_string_lossy().to_string();
            assert_eq!(
                brief_for(root.path(), &ungoverned, &a, None),
                Err(NoBrief::Ungoverned)
            );

            let who = SessionIdentity {
                session_id: "s1",
                agent_id: None,
            };
            let file = governed(root.path());
            assert!(brief_for(root.path(), &file, &a, Some(who)).is_ok());
            assert_eq!(
                brief_for(root.path(), &file, &a, Some(who)),
                Err(NoBrief::AlreadyBriefed),
                "a second read of the same file is suppressed, not ungoverned"
            );
        });
    }

    /// Without a session there is no memory, so a repeat read is answered
    /// again and `--file` never reports `already-briefed`.
    #[test]
    fn test_without_a_session_a_repeat_read_is_still_governed() {
        with_scratch_config(|| {
            let root = repo();
            let a = args(root.path());
            let file = governed(root.path());
            assert!(brief_for(root.path(), &file, &a, None).is_ok());
            assert!(brief_for(root.path(), &file, &a, None).is_ok());
        });
    }

    #[test]
    fn test_a_path_outside_the_root_says_so() {
        with_scratch_config(|| {
            let root = repo();
            assert_eq!(
                brief_for(root.path(), "/etc/hosts", &args(root.path()), None),
                Err(NoBrief::OutsideRoot)
            );
        });
    }

    /// A repository with no rule set at all is `ungoverned`, not `no-rules`:
    /// an absent directory resolves to an empty index, which searches fine and
    /// claims nothing. `no-rules` is for an index that would not build.
    #[test]
    fn test_a_repo_with_no_rule_set_governs_nothing() {
        with_scratch_config(|| {
            let bare = tempdir().unwrap();
            let file = bare.path().join("src/a.ts").to_string_lossy().to_string();
            assert_eq!(
                brief_for(bare.path(), &file, &args(bare.path()), None),
                Err(NoBrief::Ungoverned)
            );
        });
    }

    #[test]
    fn test_a_rule_set_that_will_not_build_says_so() {
        with_scratch_config(|| {
            let bare = tempdir().unwrap();
            let not_a_dir = bare.path().join("rules-file");
            std::fs::write(&not_a_dir, "not a directory").unwrap();
            let mut a = args(bare.path());
            a.rules_dir = Some(not_a_dir);
            let file = bare.path().join("src/a.ts").to_string_lossy().to_string();
            assert_eq!(
                brief_for(bare.path(), &file, &a, None),
                Err(NoBrief::NoRules)
            );
        });
    }

    /// A budget too small to hold any rule is not the same as no rule.
    #[test]
    fn test_a_budget_that_fits_nothing_says_so() {
        with_scratch_config(|| {
            let root = repo();
            let mut a = args(root.path());
            a.max_chars = 1;
            assert_eq!(
                brief_for(root.path(), &governed(root.path()), &a, None),
                Err(NoBrief::NothingToShow)
            );
        });
    }

    /// Governs `ops/` through its `Verify` path alone: `ops/deploy.sh` shares
    /// only `ops` with it, too few words for the text retry to find it.
    const BASE_IMAGES: &str = "# Pin Base Images: Container Builds\n\nThese rules are ALWAYS ACTIVE for container builds.\n\n### Rules\n\n- **R-K-001** MUST: pin every base image by digest.\n\n### Verify\n\n```bash\ngrep -rn \"FROM\" ops/\n```\n";

    /// `--limit 0` is a budget of no decisions, as a `--max-chars` too small
    /// for any rule is. A governed file has nothing to show and is still
    /// governed, whichever search found its decision, so its folder is
    /// covered rather than a gap, and nothing counts as added.
    #[test]
    fn test_a_limit_of_zero_shows_nothing_and_still_covers_the_folder() {
        with_scratch_config(|| {
            let who = SessionIdentity {
                session_id: "s1",
                agent_id: None,
            };
            let by_path_only = tempdir().unwrap();
            let dir = crate::rules::rules_dir(by_path_only.path());
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("pin-base-images-5c2a.md"), BASE_IMAGES).unwrap();
            // The premise: only the path search finds this decision.
            let index = scope::resolve_in(&dir, by_path_only.path(), false)
                .unwrap()
                .index;
            let by_text = Query::new("ops deploy.sh")
                .with_paths(["ops/deploy.sh".to_string()])
                .with_min_score(0.0);
            assert!(retry_by_text(&index, &by_text).is_empty());

            for (root, file, folder) in [
                (by_path_only, "ops/deploy.sh", "ops"),
                (
                    pathless_repo(),
                    "app/components/reading-progress/ProgressBar.tsx",
                    "app/components/reading-progress",
                ),
            ] {
                let mut a = args(root.path());
                a.limit = 0;
                let path = |file: &str| root.path().join(file).to_string_lossy().to_string();

                assert_eq!(
                    brief_for(root.path(), &path(file), &a, Some(who)),
                    Err(NoBrief::NothingToShow),
                    "{file}"
                );
                assert_eq!(
                    brief_for(root.path(), &path("infra/terraform/main.tf"), &a, Some(who)),
                    Err(NoBrief::Ungoverned),
                    "{file}"
                );
                assert_eq!(
                    folders(root.path(), "s1"),
                    (
                        vec![folder.to_string()],
                        vec!["infra/terraform".to_string()]
                    ),
                    "{file}"
                );
                assert!(ledger(root.path(), "s1").is_empty(), "{file}");
            }
        });
    }

    /// The line a reader matches on. The tokens only matter as they appear
    /// here; prose around them may change.
    #[test]
    fn test_the_no_brief_line_names_the_reason() {
        for (reason, token) in [
            (NoBrief::OutsideRoot, "outside-root"),
            (NoBrief::NoRules, "no-rules"),
            (NoBrief::Ungoverned, "ungoverned"),
            (NoBrief::AlreadyBriefed, "already-briefed"),
            (NoBrief::NothingToShow, "nothing-to-show"),
        ] {
            assert_eq!(
                report_line(reason, "src/a.ts"),
                format!("actual rules brief: no-brief={token} src/a.ts")
            );
        }
    }

    /// Direct mode prints the sentence on stdout, so each reason's wording is
    /// pinned the way the stderr line is.
    #[test]
    fn test_the_no_brief_sentence_names_the_reason() {
        for (reason, expected) in [
            (
                NoBrief::OutsideRoot,
                "src/a.ts is not under the repository root.",
            ),
            (NoBrief::NoRules, "No rule set to search for src/a.ts."),
            (NoBrief::Ungoverned, "No rule document governs src/a.ts."),
            (
                NoBrief::AlreadyBriefed,
                "Every rule document governing src/a.ts was briefed earlier this session.",
            ),
            (
                NoBrief::NothingToShow,
                "No rule text fits the brief budget for src/a.ts.",
            ),
        ] {
            assert_eq!(reason.sentence("src/a.ts"), expected);
        }
    }

    /// Hook mode stays silent on stdout for every reason, because stdout is
    /// what the model reads.
    #[test]
    fn test_every_reason_leaves_stdout_empty_in_hook_mode() {
        with_scratch_config(|| {
            let root = repo();
            let a = args(root.path());

            let ungoverned = root.path().join("README.md").to_string_lossy().to_string();
            let read = session_envelope(root.path(), "s-quiet", None, &ungoverned);
            assert_eq!(hook_reply(&read, &a), None, "ungoverned answered on stdout");

            let read = session_envelope(root.path(), "s-quiet", None, "/etc/hosts");
            assert_eq!(
                hook_reply(&read, &a),
                None,
                "outside-root answered on stdout"
            );

            let file = governed(root.path());
            let read = session_envelope(root.path(), "s-quiet", None, &file);
            assert!(hook_reply(&read, &a).is_some(), "first read must brief");
            assert_eq!(
                hook_reply(&read, &a),
                None,
                "already-briefed answered on stdout"
            );

            let bare = tempdir().unwrap();
            let not_a_dir = bare.path().join("rules-file");
            std::fs::write(&not_a_dir, "not a directory").unwrap();
            let mut no_rules = args(bare.path());
            no_rules.rules_dir = Some(not_a_dir);
            let other = bare.path().join("src/a.ts").to_string_lossy().to_string();
            let read = session_envelope(bare.path(), "s-quiet", None, &other);
            assert_eq!(
                hook_reply(&read, &no_rules),
                None,
                "no-rules answered on stdout"
            );

            let mut tiny = args(root.path());
            tiny.max_chars = 1;
            let read = session_envelope(root.path(), "s-tiny", None, &file);
            assert_eq!(
                hook_reply(&read, &tiny),
                None,
                "nothing-to-show answered on stdout"
            );
        });
    }

    /// The record counts by the memory's rule. A decision `--max-chars` cut
    /// short is not counted, because a later read can still show it whole;
    /// one trimmed to `--rules-per-decision` is, because that is all it can
    /// ever show.
    #[test]
    fn test_the_session_record_counts_by_the_memorys_rule() {
        with_scratch_config(|| {
            let root = repo();
            std::fs::write(
                crate::rules::rules_dir(root.path()).join("cross-cutting-token-signing-e410.md"),
                OAUTH.replace(
                    "- **R-A-002** SHOULD: rotate keys quarterly.",
                    "- **R-A-002** MUST NOT: sign with HS256.",
                ),
            )
            .unwrap();
            let file = governed(root.path());
            let read = session_envelope(root.path(), "s1", None, &file);
            let whole = brief_for(root.path(), &file, &args(root.path()), None).unwrap();

            let mut cut = args(root.path());
            cut.max_chars = whole.chars().count() - 1;
            let brief = hook_reply(&read, &cut).expect("a shortened brief");
            assert!(brief.contains("(1 of 2 rules shown)"), "{brief}");
            assert!(ledger(root.path(), "s1").is_empty());

            let mut capped = args(root.path());
            capped.rules_per_decision = 1;
            assert!(hook_reply(&read, &capped).is_some());
            assert_eq!(ledger(root.path(), "s1"), ["Sign With Asymmetric Keys"]);
        });
    }

    // ── path resolution ──────────────────────────────────────────────────

    /// The envelope's `cwd` stands in for the repository root when neither
    /// `--repo` nor a conventional `--rules-dir` names it.
    #[test]
    fn test_cwd_from_the_envelope_is_the_repository_root() {
        let root = repo();
        let mut a = args(root.path());
        a.repo = None;
        let raw = envelope(root.path(), "PostToolUse", "Read", &governed(root.path()));

        assert!(hook_reply(&raw, &a).is_some());
    }

    /// `--repo` wins; without it a rules directory in the conventional place
    /// names the root; anything else falls back.
    #[test]
    fn test_the_root_is_the_repo_then_the_rules_dirs_root_then_the_fallback() {
        let with = |repo: Option<&str>, rules_dir: Option<&str>| RulesBriefArgs {
            repo: repo.map(PathBuf::from),
            rules_dir: rules_dir.map(PathBuf::from),
            ..args(Path::new("/unused"))
        };
        for (repo, rules_dir, expected) in [
            (Some("/repo"), Some("/other/.actual/rules"), "/repo"),
            (None, Some("/repo/.actual/rules"), "/repo"),
            (
                None,
                Some("/repo/apps/web/.actual/rules/"),
                "/repo/apps/web",
            ),
            (None, Some("/repo/custom-rules"), "/fallback"),
            (None, Some(".actual/rules"), "/fallback"),
            (None, None, "/fallback"),
        ] {
            assert_eq!(
                repo_root(&with(repo, rules_dir), || PathBuf::from("/fallback")),
                Path::new(expected),
                "{repo:?} {rules_dir:?}"
            );
        }
    }

    /// After the agent `cd`s into a subfolder, a hook given `--rules-dir` and
    /// no `--repo` still places, briefs and records files from the repository
    /// root rather than from the envelope's `cwd`.
    #[test]
    fn test_a_subfolder_cwd_still_places_files_from_the_repository_root() {
        with_scratch_config(|| {
            let root = repo();
            let mut a = args(root.path());
            a.repo = None;
            a.rules_dir = Some(crate::rules::rules_dir(root.path()));
            let read = |file: &str| {
                let mut value: serde_json::Value =
                    serde_json::from_str(&read_in(root.path(), "s1", file)).unwrap();
                value["cwd"] = root.path().join("services").to_string_lossy().into();
                value.to_string()
            };

            let brief = hook_reply(&read("services/auth/oauth/token.ts"), &a).expect("a brief");
            assert!(brief.contains("`services/auth/oauth/token.ts`"), "{brief}");
            assert_eq!(hook_reply(&read("infra/terraform/main.tf"), &a), None);

            assert_eq!(
                folders(root.path(), "s1"),
                (
                    vec!["services/auth/oauth".to_string()],
                    vec!["infra/terraform".to_string()]
                )
            );
        });
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

    /// A glob match the floor filtered out can come back through the text
    /// retry when the file's path shares enough words with the rule: the floor
    /// is judged on the retry's fuller score.
    #[test]
    fn test_the_retry_briefs_a_glob_match_the_floor_dropped_on_enough_words() {
        let root = repo();
        let mut a = args(root.path());
        a.min_score = Some(2.5);
        let raw = envelope(root.path(), "PostToolUse", "Read", &governed(root.path()));

        let out = hook_reply(&raw, &a).expect("a brief");

        assert!(out.contains("Sign With Asymmetric Keys"), "{out}");
    }

    /// The same drop is not undone by one shared word: a glob match does not
    /// exempt a document from the retry's two-word minimum.
    #[test]
    fn test_the_retry_does_not_rescue_a_glob_match_on_one_word() {
        let root = tempdir().unwrap();
        let dir = crate::rules::rules_dir(root.path());
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("release-checklist-5d2a.md"), RELEASE_NOTES).unwrap();
        let file = root.path().join("Notes/a.md").to_string_lossy().to_string();
        let raw = envelope(root.path(), "PostToolUse", "Read", &file);
        let mut a = args(root.path());

        assert!(hook_reply(&raw, &a).is_some(), "briefed with no floor");
        a.min_score = Some(1.0);
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
