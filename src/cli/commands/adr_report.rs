//! The Stop hook's question to the coding agent, "which of these ADRs did you
//! use this turn?", and `actual rules report-use`, which records the answer
//! and checks each claim against the turn's work.
//!
//! # Design
//!
//! **Asked once per turn, at the first check of a non-empty diff.**
//! `impl-check --claude-hook` [`prepare`]s the question before it judges and
//! [`mark`]s the turn asked just before it prints, so every later Stop in the
//! turn, including the one after the answer, asks nothing. A turn is the
//! session's [`GovernanceSession::turn`], counted on `UserPromptSubmit`. A
//! diff that still matches the one recorded when the turn started holds only
//! earlier turns' uncommitted work, so that turn is not asked.
//!
//! **The list is the brief ledger, unfiltered.** It names every decision that
//! reached the session through `rules brief`, which is keyed on the session
//! alone, so subagents and compactions are covered and the gate's selector
//! plays no part. An empty ledger asks nothing.
//!
//! **The check is deterministic.** A claim holds when its ADR was in the
//! question and its line is one this turn added or changed, or one of the
//! plan `plan-check` judged this turn. A diff claim a few lines off still
//! holds on the nearest added line in the same hunk, within
//! [`LINE_TOLERANCE`], and records the line it matched. Anything else is
//! unverified, with the first check it failed as the reason. Verified means
//! the claim points at this turn's work, not that the ADR shaped it: any
//! listed ADR verifies on any changed line.
//!
//! **Asked only when the answer is reported.** Nothing local reads the
//! answer, so with telemetry off (and in a build without the `telemetry`
//! feature) there is no question, and `plan-check` keeps no plan for one.
//! Debug mode still asks, because it logs the records it would have sent.
//!
//! **Only the verdict leaves the machine.** The claims, the lines they quote
//! and anything the agent wrote stay in the session file under the config
//! directory. [`otlp::ADR_REPORTED`] carries the ADR, the turn, whether the
//! claim held and why not, and, for a verified diff claim only, the file
//! path, which only a signed-in request keeps.
//!
//! **Flags as well as stdin.** Claude Code runs the answer as a Bash command,
//! and stdin there takes a pipe or a redirect, which the skill's permission
//! hook refuses. So the question names `--claim` and `--none`, and JSON on
//! stdin stays for scripts.

use std::collections::BTreeMap;
use std::io::IsTerminal;
use std::path::{Component, Path};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::cli::args::RulesReportUseArgs;
use crate::cli::commands::brief_ledger::{self, LedgerKey};
use crate::cli::commands::check_engine::capped_read;
use crate::cli::commands::governance_session::{self, GovernanceSession};
use crate::cli::commands::impl_check;
use crate::cli::commands::plan_check::repo_root;
use crate::error::ActualError;
use crate::rules::scope::index::ScopeIndex;

/// Most claims one answer may carry.
const MAX_CLAIMS: usize = 50;

/// Longest quoted line or note kept in the session file, in characters.
const MAX_KEPT_CHARS: usize = 500;

/// Longest ADR title shown in the question, in characters.
const MAX_LABEL_CHARS: usize = 120;

/// The question's first paragraph, which says what counts as used.
const QUESTION_HEAD: &str = "Which of these ADRs did you use this turn? \
     An ADR counts only if it changed or decided what you wrote. \
     Leave out any your change merely complies with or happens to agree with.";

/// How far, in lines, a diff claim may miss the added or changed line it
/// means, within one hunk.
const LINE_TOLERANCE: u64 = 3;

/// Longest repository path a claim may name, in bytes.
const MAX_PATH_BYTES: usize = 1024;

/// A turn's question, and the answer once given.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdrAsk {
    pub turn: u64,
    /// What the question told the agent to pass as `--turn`.
    pub token: String,
    /// The `actual.action.id` of the `impl-check` run that asked.
    #[serde(default)]
    pub action_id: String,
    pub adrs: Vec<AskedAdr>,
    /// The digest of the diff the asking round judged.
    #[serde(default)]
    pub diff_digest: String,
    /// Whether the asking round denied, so the question rode on a block.
    #[serde(default)]
    pub on_block: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<Answer>,
}

impl AdrAsk {
    /// The ADR `name` picks out: its handle, a file's slug, an id, or the key.
    fn find(&self, name: &str) -> Option<&AskedAdr> {
        self.adrs
            .iter()
            .find(|adr| adr.handle == name || adr.aliases.iter().any(|alias| alias == name))
    }
}

/// One decision the question listed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AskedAdr {
    /// What the question calls it.
    pub handle: String,
    /// Its brief ledger key.
    pub key: String,
    /// Every other name a claim may use for it.
    #[serde(default)]
    pub aliases: Vec<String>,
    /// The logical ADR telemetry names: the candidate group, else the first
    /// file's `adr_id`. Absent for a decision with no v2 header.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adr_ref: Option<String>,
    #[serde(default)]
    pub adr_ids: Vec<String>,
}

/// The agent's answer, as checked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Answer {
    pub at: DateTime<Utc>,
    pub claims: Vec<CheckedClaim>,
}

/// One claim and what the check made of it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckedClaim {
    /// The ADR as the agent named it.
    pub adr: String,
    pub anchor: Anchor,
    /// The repository path a diff anchor names.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    pub line: u64,
    /// The added or changed line a diff claim verified on, when it is not
    /// `line` itself but one near it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matched_line: Option<u64>,
    /// `matched_line - line`, when there is a matched line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_offset: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<Unverified>,
    /// The text of the line the claim verified on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quote: Option<String>,
    /// What the agent wrote beside the claim, from JSON input.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl CheckedClaim {
    #[cfg_attr(not(feature = "telemetry"), allow(dead_code))]
    fn verified(&self) -> bool {
        self.reason.is_none()
    }
}

/// What a claim's line is a line of.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Anchor {
    Diff,
    Plan,
}

impl Anchor {
    #[cfg_attr(not(feature = "telemetry"), allow(dead_code))]
    fn as_str(self) -> &'static str {
        match self {
            Self::Diff => "diff",
            Self::Plan => "plan",
        }
    }
}

/// Why a claim did not hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Unverified {
    /// The ADR was not one the question listed.
    AdrNotInSession,
    /// The file has no change this turn, or no plan was judged this turn.
    FileNotChanged,
    /// The line is not one this turn added or changed, or not a line of the plan.
    LineNotInDiff,
}

impl Unverified {
    #[cfg_attr(not(feature = "telemetry"), allow(dead_code))]
    fn as_str(self) -> &'static str {
        match self {
            Self::AdrNotInSession => "adr_not_in_session",
            Self::FileNotChanged => "file_not_changed",
            Self::LineNotInDiff => "line_not_in_diff",
        }
    }
}

/// The plan `plan-check` judged, and the turn it judged it in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnPlan {
    pub turn: u64,
    /// The plan's text, dropped once the turn's answer is recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default)]
    pub lines: usize,
}

// ── the question ─────────────────────────────────────────────────────────

/// A question ready to ask, if the round turns out to be the turn's first.
#[derive(Debug, Clone)]
pub struct PreparedAsk {
    turn: u64,
    token: String,
    diff_digest: String,
    adrs: Vec<AskedAdr>,
    text: String,
}

impl PreparedAsk {
    pub(super) fn text(&self) -> &str {
        &self.text
    }
}

/// The question for this round, or `None` when the turn was already asked,
/// there is no session, nothing was briefed in it, the answer would not be
/// reported, or the diff from `base` is the one the turn started with. A
/// diff taken from any other base than the turn's recorded commit is never
/// read as unchanged.
pub(super) fn prepare(
    session_id: Option<&str>,
    session: &GovernanceSession,
    root: &Path,
    rules_dir: &Path,
    base: Option<&str>,
    diff_digest: &str,
) -> Option<PreparedAsk> {
    let session_id = session_id.filter(|id| !id.is_empty())?;
    let unchanged = session.turn_start.as_ref().is_some_and(|start| {
        base == Some(start.head.as_str()) && start.diff_digest.as_deref() == Some(diff_digest)
    });
    let asked = session
        .adr_ask
        .as_ref()
        .is_some_and(|ask| ask.turn == session.turn);
    if unchanged || asked || !reported() {
        return None;
    }
    let ledger_rules_dir = super::rules_brief::resolve(rules_dir);
    let ledger = brief_ledger::load(
        &brief_ledger::ledger_dir()?,
        &LedgerKey {
            session_id,
            rules_dir: &ledger_rules_dir,
        },
    );
    if ledger.briefed.is_empty() {
        return None;
    }
    let index = crate::rules::scope::resolve_in(rules_dir, root, false)
        .ok()
        .map(|resolved| resolved.index);
    let listed = asked_adrs(ledger.briefed.iter(), index.as_ref());
    let token = format!(
        "{}-{}",
        session.turn,
        diff_digest.get(..8).unwrap_or(diff_digest)
    );
    let text = question(session_id, &token, root, rules_dir, &listed);
    Some(PreparedAsk {
        turn: session.turn,
        token,
        diff_digest: diff_digest.to_string(),
        adrs: listed.into_iter().map(|(adr, _)| adr).collect(),
        text,
    })
}

/// Record `ask` as this turn's question, unless one already was. True only
/// when this call recorded it and the session reads it back, so a session
/// that cannot be written never asks, and never asks again after its answer.
pub(super) fn mark(session_id: &str, rules_dir: &Path, ask: &PreparedAsk, on_block: bool) -> bool {
    let mut marked = false;
    governance_session::update(session_id, rules_dir, |session| {
        let asked = session
            .adr_ask
            .as_ref()
            .is_some_and(|recorded| recorded.turn == ask.turn);
        if session.turn != ask.turn || asked {
            return;
        }
        session.adr_ask = Some(AdrAsk {
            turn: ask.turn,
            token: ask.token.clone(),
            action_id: action_id(),
            adrs: ask.adrs.clone(),
            diff_digest: ask.diff_digest.clone(),
            on_block,
            answer: None,
        });
        marked = true;
    });
    marked
        && governance_session::load(session_id, rules_dir)
            .adr_ask
            .is_some_and(|recorded| recorded.token == ask.token)
}

/// Whether this turn's question was asked on a round that let the diff
/// through, and the diff is unchanged since: the answer changed no file, so
/// the Stop after it has nothing new to judge. A rule a round limit waved
/// through stays uncleared, and would otherwise be judged again here. A
/// question asked on a block never settles the diff, so the block stands
/// until the diff changes.
pub(super) fn settled(session: &GovernanceSession, diff_digest: &str) -> bool {
    session.adr_ask.as_ref().is_some_and(|ask| {
        ask.turn == session.turn && !ask.on_block && ask.diff_digest == diff_digest
    })
}

#[cfg(feature = "telemetry")]
fn action_id() -> String {
    crate::telemetry::otlp::current_action_id().to_string()
}

#[cfg(not(feature = "telemetry"))]
fn action_id() -> String {
    String::new()
}

/// Whether an answer would reach [`report`]: the opt-out gate is not
/// [`Gate::Off`](crate::telemetry::opt_out::Gate::Off).
#[cfg(feature = "telemetry")]
fn reported() -> bool {
    !matches!(
        crate::telemetry::opt_out::quiet_gate(),
        crate::telemetry::opt_out::Gate::Off
    )
}

#[cfg(not(feature = "telemetry"))]
fn reported() -> bool {
    false
}

/// Each briefed decision as the question lists it, with its title, sorted by
/// handle. A decision the rule set no longer holds is listed by its key.
fn asked_adrs<'a>(
    briefed: impl Iterator<Item = &'a String>,
    index: Option<&ScopeIndex>,
) -> Vec<(AskedAdr, String)> {
    let mut listed: Vec<(AskedAdr, String)> = briefed
        .map(|key| {
            let documents: Vec<_> = index
                .into_iter()
                .flat_map(|index| index.documents.iter())
                .filter(|document| document.decision_key() == *key)
                .collect();
            let mut aliases = vec![key.clone()];
            let mut adr_ids: Vec<String> = Vec::new();
            for document in &documents {
                aliases.push(document.slug.clone());
                if let Some(id) = document.adr_id.as_deref().filter(|id| !id.is_empty()) {
                    aliases.push(id.to_string());
                    if !adr_ids.iter().any(|known| known == id) {
                        adr_ids.push(id.to_string());
                    }
                }
                aliases.extend(document.candidate_group_id.clone());
            }
            let group = documents
                .iter()
                .find_map(|document| document.candidate_group_id.clone());
            let adr_ref = group.or_else(|| adr_ids.first().cloned());
            let handle = documents
                .first()
                .map_or_else(|| key.clone(), |document| document.slug.clone());
            let label = documents
                .first()
                .and_then(|document| document.adr.clone().or_else(|| document.title.clone()))
                .unwrap_or_else(|| handle.clone());
            aliases.sort();
            aliases.dedup();
            let adr = AskedAdr {
                handle,
                key: key.clone(),
                aliases,
                adr_ref,
                adr_ids,
            };
            (adr, clean(&label, MAX_LABEL_CHARS))
        })
        .collect();
    listed.sort_by(|a, b| a.0.handle.cmp(&b.0.handle));
    listed
}

/// The question's text: the ADRs, then the one command that answers it.
fn question(
    session_id: &str,
    token: &str,
    root: &Path,
    rules_dir: &Path,
    adrs: &[(AskedAdr, String)],
) -> String {
    let list: Vec<String> = adrs
        .iter()
        .map(|(adr, label)| format!("- `{}`: {label}", adr.handle))
        .collect();
    let mut command = format!(
        "actual rules report-use --claude-session {} --turn {} --repo {}",
        shell_word(session_id),
        shell_word(token),
        shell_word(&root.to_string_lossy()),
    );
    if rules_dir != crate::rules::rules_dir(root) {
        command.push_str(" --rules-dir ");
        command.push_str(&shell_word(&rules_dir.to_string_lossy()));
    }
    format!(
        "{QUESTION_HEAD}\n\n\
         {}\n\n\
         Answer with one command, then end your turn:\n\
         {command} --claim <adr>=<repo-relative-path>:<line>\n\n\
         Repeat --claim for each ADR you used, naming the line you added or changed this turn that best shows its effect. \
         For a line of the plan approved this turn, use --plan-claim <adr>=<line>. \
         When no listed ADR shaped the change, run the command with --none in place of the claims. \
         That is a normal, expected answer.",
        list.join("\n")
    )
}

/// `text` with control characters and runs of whitespace folded to one space,
/// cut to `max` characters.
fn clean(text: &str, max: usize) -> String {
    let folded: String = text
        .split(|c: char| c.is_whitespace() || c.is_control())
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    folded.chars().take(max).collect()
}

/// `word` as one shell word: as is when every character is plain, else single-quoted.
fn shell_word(word: &str) -> String {
    if !word.is_empty() && word.chars().all(is_plain) {
        word.to_string()
    } else {
        format!("'{}'", word.replace('\'', r"'\''"))
    }
}

fn is_plain(c: char) -> bool {
    c.is_ascii_alphanumeric() || "._/:=@+,-".contains(c)
}

/// Keep `plan` as the plan judged in this session's current turn, unless no
/// question will be asked for it to answer.
pub(super) fn keep_plan(session_id: &str, rules_dir: &Path, plan: &str) {
    if !reported() {
        return;
    }
    governance_session::update(session_id, rules_dir, |session| {
        session.turn_plan = Some(TurnPlan {
            turn: session.turn,
            text: Some(plan.to_string()),
            lines: plan.lines().count(),
        });
    });
}

// ── the answer ───────────────────────────────────────────────────────────

/// One claim as given.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Claim {
    adr: String,
    anchor: Anchor,
    file: Option<String>,
    line: u64,
    note: Option<String>,
}

/// A claim in JSON input: `{"adr": .., "file": .., "line": ..}` for a diff
/// line, `{"adr": .., "plan_line": ..}` for a plan line, either with a `note`.
#[derive(Deserialize)]
struct JsonClaim {
    adr: String,
    #[serde(default)]
    file: Option<String>,
    #[serde(default)]
    line: Option<u64>,
    #[serde(default)]
    plan_line: Option<u64>,
    #[serde(default)]
    note: Option<String>,
}

pub fn exec(args: &RulesReportUseArgs) -> Result<(), ActualError> {
    let stdin = std::io::stdin();
    let piped = (!stdin.is_terminal()).then_some(stdin);
    answer(args, piped).map(|line| println!("{line}"))
}

/// [`exec_with`], reading the claims from `piped` when no flag gives them.
fn answer(
    args: &RulesReportUseArgs,
    piped: Option<impl std::io::Read>,
) -> Result<String, ActualError> {
    if !args.claims.is_empty() || !args.plan_claims.is_empty() || args.none {
        return exec_with(args, None);
    }
    let input = piped.ok_or_else(|| {
        usage("give --claim, --plan-claim or --none, or the claims as JSON on stdin")
    })?;
    exec_with(args, Some(&capped_read(input, "the claims on stdin")?))
}

/// Record and check the answer, returning the line to print.
fn exec_with(args: &RulesReportUseArgs, stdin: Option<&str>) -> Result<String, ActualError> {
    let claims = match stdin {
        Some(json) => json_claims(json)?,
        None => flag_claims(args)?,
    };
    if claims.len() > MAX_CLAIMS {
        return Err(usage(&format!("at most {MAX_CLAIMS} claims per answer")));
    }
    let root = repo_root(args.repo.as_ref());
    let rules_dir = args
        .rules_dir
        .clone()
        .unwrap_or_else(|| crate::rules::rules_dir(&root));
    let session_id = args.claude_session.as_str();
    #[cfg(feature = "telemetry")]
    crate::telemetry::debug::use_session(session_id);

    let session = governance_session::load(session_id, &rules_dir);
    let Some(ask) = session
        .adr_ask
        .as_ref()
        .filter(|ask| ask.token == args.turn && ask.turn == session.turn)
    else {
        return Err(ActualError::ConfigError(format!(
            "no ADR question is open for session {session_id} at turn {}; nothing recorded",
            args.turn
        )));
    };
    let base = impl_check::turn_base(&session, &root);
    let diff = impl_check::working_tree_diff_from(&root, base.as_deref().unwrap_or("HEAD"))?;
    let changed = changed_lines(&diff);
    let plan = session
        .turn_plan
        .as_ref()
        .filter(|plan| plan.turn == ask.turn)
        .and_then(|plan| plan.text.as_deref());
    let checkout = impl_check::checkout_root(&root);
    let checked: Vec<CheckedClaim> = claims
        .into_iter()
        .map(|claim| check(claim, ask, &changed, plan, &checkout))
        .collect();

    let mut stored = false;
    governance_session::update(session_id, &rules_dir, |session| {
        if let Some(open) = session
            .adr_ask
            .as_mut()
            .filter(|open| open.token == args.turn && open.answer.is_none())
        {
            open.answer = Some(Answer {
                at: Utc::now(),
                claims: checked.clone(),
            });
            stored = true;
        }
        if stored {
            if let Some(plan) = session.turn_plan.as_mut() {
                plan.text = None;
            }
        }
    });
    if !stored {
        return Ok(ALREADY.to_string());
    }
    #[cfg(feature = "telemetry")]
    report(session_id, ask, &checked);
    Ok(acknowledgement(&checked))
}

const ALREADY: &str = "This turn's answer is already recorded; nothing changed.";

fn usage(message: &str) -> ActualError {
    ActualError::ConfigError(format!("rules report-use: {message}"))
}

fn acknowledgement(claims: &[CheckedClaim]) -> String {
    match claims.len() {
        0 => "Recorded that no ADR shaped this turn's change. You can end your turn.".to_string(),
        1 => "Recorded 1 ADR claim for this turn. You can end your turn.".to_string(),
        n => format!("Recorded {n} ADR claims for this turn. You can end your turn."),
    }
}

/// The claims `--claim`, `--plan-claim` and `--none` give.
fn flag_claims(args: &RulesReportUseArgs) -> Result<Vec<Claim>, ActualError> {
    let diff = args.claims.iter().map(|claim| {
        let parsed = claim.split_once('=').and_then(|(adr, rest)| {
            let (file, line) = rest.rsplit_once(':')?;
            Some((adr, Some(file), line))
        });
        (claim, parsed, Anchor::Diff)
    });
    let plan = args.plan_claims.iter().map(|claim| {
        let parsed = claim
            .split_once('=')
            .map(|(adr, line)| (adr, None::<&str>, line));
        (claim, parsed, Anchor::Plan)
    });
    diff.chain(plan)
        .map(|(given, parsed, anchor)| {
            let claim = parsed.and_then(|(adr, file, line)| {
                Some(Claim {
                    adr: adr.trim().to_string(),
                    anchor,
                    file: file.map(str::to_string),
                    line: line.trim().parse().ok().filter(|line| *line > 0)?,
                    note: None,
                })
            });
            claim
                .filter(|claim| !claim.adr.is_empty() && claim.file.as_deref() != Some(""))
                .ok_or_else(|| {
                    usage(&format!(
                        "cannot read {given:?}: write --claim <adr>=<repo-relative-path>:<line> or --plan-claim <adr>=<line>"
                    ))
                })
        })
        .collect()
}

/// The claims in a JSON array on stdin. An empty array means no ADR was used.
fn json_claims(json: &str) -> Result<Vec<Claim>, ActualError> {
    let given: Vec<JsonClaim> = serde_json::from_str(json)
        .map_err(|e| usage(&format!("stdin is not a JSON array of claims: {e}")))?;
    given
        .into_iter()
        .map(|claim| {
            let note = claim
                .note
                .as_deref()
                .map(|note| clean(note, MAX_KEPT_CHARS))
                .filter(|note| !note.is_empty());
            let anchored = match (claim.file, claim.line, claim.plan_line) {
                (Some(file), Some(line), None) if !file.is_empty() => {
                    Some((Anchor::Diff, Some(file), line))
                }
                (None, None, Some(line)) => Some((Anchor::Plan, None, line)),
                _ => None,
            };
            anchored
                .filter(|(_, _, line)| *line > 0 && !claim.adr.trim().is_empty())
                .map(|(anchor, file, line)| Claim {
                    adr: claim.adr.trim().to_string(),
                    anchor,
                    file,
                    line,
                    note,
                })
                .ok_or_else(|| {
                    usage("each claim needs an adr and either a file and line, or a plan_line")
                })
        })
        .collect()
}

/// Check one claim: the ADR first, then the file, then the line.
fn check(
    claim: Claim,
    ask: &AdrAsk,
    changed: &BTreeMap<String, ChangedFile>,
    plan: Option<&str>,
    checkout: &Path,
) -> CheckedClaim {
    let file = claim
        .file
        .as_deref()
        .and_then(|file| repository_path(file, checkout));
    let line = |text: &str| Some(clean(text, MAX_KEPT_CHARS));
    let mut matched_line = None;
    let (reason, quote) = if ask.find(&claim.adr).is_none() {
        (Some(Unverified::AdrNotInSession), None)
    } else {
        match (claim.anchor, file.as_deref(), plan) {
            (Anchor::Diff, Some(file), _) => match changed.get(file) {
                None => (Some(Unverified::FileNotChanged), None),
                Some(changed) => match changed.nearest(claim.line) {
                    Some((matched, text)) => {
                        matched_line = (matched != claim.line).then_some(matched);
                        (None, line(text))
                    }
                    None => (Some(Unverified::LineNotInDiff), None),
                },
            },
            (Anchor::Plan, _, Some(plan)) => {
                match usize::try_from(claim.line - 1)
                    .ok()
                    .and_then(|index| plan.lines().nth(index))
                    .filter(|text| !text.trim().is_empty())
                {
                    Some(text) => (None, line(text)),
                    None => (Some(Unverified::LineNotInDiff), None),
                }
            }
            _ => (Some(Unverified::FileNotChanged), None),
        }
    };
    CheckedClaim {
        adr: claim.adr,
        anchor: claim.anchor,
        file,
        line: claim.line,
        matched_line,
        line_offset: matched_line.map(|matched| offset(claim.line, matched)),
        reason,
        quote,
        note: claim.note,
    }
}

/// `to - from` as a signed line count.
fn offset(from: u64, to: u64) -> i64 {
    let distance = i64::try_from(from.abs_diff(to)).unwrap_or(i64::MAX);
    if to >= from {
        distance
    } else {
        -distance
    }
}

/// `file` relative to the checkout, `/`-separated: a relative path is read
/// from the checkout root, never the working directory, an absolute path
/// inside it loses the checkout's prefix, resolved through symlinks when it
/// must be, and `.` segments go. `None` for a path outside the checkout, one
/// with a `..` segment, and one longer than [`MAX_PATH_BYTES`].
fn repository_path(file: &str, checkout: &Path) -> Option<String> {
    let path = checkout.join(file);
    let resolved = path
        .parent()
        .and_then(|parent| parent.canonicalize().ok())
        .zip(path.file_name())
        .map(|(parent, name)| parent.join(name));
    let relative = path
        .strip_prefix(checkout)
        .ok()
        .or_else(|| resolved.as_deref()?.strip_prefix(checkout).ok())
        .unwrap_or(&path);
    let mut parts = Vec::new();
    for component in relative.components() {
        match component {
            Component::Normal(part) => parts.push(part.to_string_lossy()),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    Some(parts.join("/")).filter(|joined| !joined.is_empty() && joined.len() <= MAX_PATH_BYTES)
}

/// One file's lines this turn added, and the new-side lines each hunk spans.
#[derive(Debug, Default, PartialEq, Eq)]
struct ChangedFile {
    /// Each added line's text and the index of its hunk, by line number.
    lines: BTreeMap<u64, (usize, String)>,
    /// Each hunk's new-side lines, context included, as `first..end`.
    hunks: Vec<std::ops::Range<u64>>,
}

impl ChangedFile {
    /// The added line a claim on `line` means, and its text: `line` itself,
    /// else the nearest added line in the same hunk at most
    /// [`LINE_TOLERANCE`] away, the earlier on a tie.
    fn nearest(&self, line: u64) -> Option<(u64, &str)> {
        if let Some((_, text)) = self.lines.get(&line) {
            return Some((line, text));
        }
        let hunk = self.hunks.iter().position(|hunk| hunk.contains(&line))?;
        self.lines
            .range(line.saturating_sub(LINE_TOLERANCE)..=line.saturating_add(LINE_TOLERANCE))
            .filter(|(_, (of, _))| *of == hunk)
            .min_by_key(|(added, _)| (added.abs_diff(line), **added))
            .map(|(added, (_, text))| (*added, text.as_str()))
    }
}

/// The lines a unified diff adds, by new-side file path and line number.
/// A file that adds none, deleted or not, is absent.
fn changed_lines(diff: &str) -> BTreeMap<String, ChangedFile> {
    let mut changed: BTreeMap<String, ChangedFile> = BTreeMap::new();
    let mut file: Option<String> = None;
    let mut next = 0u64;
    let mut in_hunk = false;
    for line in diff.lines() {
        if line.starts_with("diff --git ") {
            file = None;
            in_hunk = false;
        } else if let Some(path) = line.strip_prefix("+++ ").filter(|_| !in_hunk) {
            file = unquote(path).and_then(|path| path.strip_prefix("b/").map(str::to_string));
        } else if let Some(header) = line.strip_prefix("@@ ") {
            next = hunk_start(header).unwrap_or(0);
            in_hunk = true;
            if let Some(file) = &file {
                changed
                    .entry(file.clone())
                    .or_default()
                    .hunks
                    .push(next..next);
            }
        } else if in_hunk {
            let current = file.as_ref().and_then(|file| changed.get_mut(file));
            match line.as_bytes().first() {
                Some(b'+') => {
                    if let Some(current) = current {
                        let hunk = current.hunks.len() - 1;
                        current.lines.insert(next, (hunk, line[1..].to_string()));
                        current.hunks[hunk].end = next + 1;
                    }
                    next += 1;
                }
                Some(b' ') => {
                    if let Some(hunk) = current.and_then(|current| current.hunks.last_mut()) {
                        hunk.end = next + 1;
                    }
                    next += 1;
                }
                Some(b'-' | b'\\') => {}
                _ => in_hunk = false,
            }
        }
    }
    changed.retain(|_, file| !file.lines.is_empty());
    changed
}

/// The new side's first line in a hunk header's text after `@@ `:
/// `-a,b +c,d @@ ...` gives `c`.
fn hunk_start(header: &str) -> Option<u64> {
    let new_side = header.split_whitespace().nth(1)?.strip_prefix('+')?;
    new_side.split(',').next()?.parse().ok()
}

/// A `+++` path as git wrote it: as is, or C-quoted when it holds unusual
/// bytes. `/dev/null`, a deleted file's new side, is `None`.
fn unquote(path: &str) -> Option<String> {
    if path == "/dev/null" {
        return None;
    }
    let Some(quoted) = path.strip_prefix('"').and_then(|p| p.strip_suffix('"')) else {
        return Some(path.to_string());
    };
    let mut bytes = Vec::new();
    let mut chars = quoted.bytes().peekable();
    while let Some(byte) = chars.next() {
        if byte != b'\\' {
            bytes.push(byte);
            continue;
        }
        match chars.next()? {
            b'n' => bytes.push(b'\n'),
            b't' => bytes.push(b'\t'),
            b'r' => bytes.push(b'\r'),
            b'a' => bytes.push(7),
            b'b' => bytes.push(8),
            b'f' => bytes.push(12),
            b'v' => bytes.push(11),
            digit @ b'0'..=b'7' => {
                let mut value = u32::from(digit - b'0');
                for _ in 0..2 {
                    let next = chars.next_if(|b| (b'0'..=b'7').contains(b))?;
                    value = value * 8 + u32::from(next - b'0');
                }
                bytes.push(u8::try_from(value).ok()?);
            }
            other => bytes.push(other),
        }
    }
    String::from_utf8(bytes).ok()
}

// ── telemetry ────────────────────────────────────────────────────────────

/// One [`crate::telemetry::otlp::ADR_REPORTED`] record per listed ADR the
/// answer names, and one more for all the names the question never listed.
/// An ADR named more than once counts as verified when any of its claims
/// held, and reports the first one that did, else its first claim. An ADR
/// without a v2 header sends nothing, as it does on every ADR event. Only a
/// verified claim sends its file path.
#[cfg(feature = "telemetry")]
fn report(session_id: &str, ask: &AdrAsk, claims: &[CheckedClaim]) {
    use crate::telemetry::otlp::{AdrReport, Origin};

    let Some(destination) = super::brief_telemetry::Destination::resolve() else {
        return;
    };
    let mut per_adr: Vec<(Option<&AskedAdr>, &CheckedClaim)> = Vec::new();
    for claim in claims {
        let adr = ask.find(&claim.adr);
        match per_adr
            .iter_mut()
            .find(|(known, _)| known.map(|known| &known.key) == adr.map(|adr| &adr.key))
        {
            Some(entry) if !entry.1.verified() && claim.verified() => entry.1 = claim,
            Some(_) => {}
            None => per_adr.push((adr, claim)),
        }
    }
    let origin = Origin::resolve();
    let records: Vec<_> = per_adr
        .into_iter()
        .filter(|(adr, _)| adr.is_none_or(|adr| adr.adr_ref.is_some()))
        .map(|(adr, claim)| {
            origin.reported(
                session_id,
                &AdrReport {
                    adr: adr.and_then(|adr| {
                        adr.adr_ref
                            .as_deref()
                            .map(|adr_ref| (adr_ref, adr.adr_ids.as_slice()))
                    }),
                    turn: ask.turn,
                    check_action_id: &ask.action_id,
                    evidence: if claim.verified() {
                        "verified"
                    } else {
                        "unverified"
                    },
                    reason: claim.reason.map(Unverified::as_str),
                    anchor: claim.anchor.as_str(),
                    file_path: claim.file.as_deref().filter(|_| claim.verified()),
                },
            )
        })
        .collect();
    if !records.is_empty() {
        destination.deliver(records);
    }
}

#[cfg(test)]
#[cfg_attr(not(feature = "telemetry"), allow(dead_code))]
pub(super) mod tests {
    use super::*;

    use std::path::PathBuf;

    use crate::testutil::{EnvGuard, ENV_MUTEX};
    use tempfile::{tempdir, TempDir};

    const RULES: &str = "### Rules\n\n- **R-1** MUST: return typed errors.\n";

    /// A scratch config dir and a committed repository holding three v2 ADRs
    /// (two files of one group, and one alone) and one file without a header.
    pub(in crate::cli::commands) struct Fixture {
        _home: TempDir,
        pub repo: TempDir,
        _guards: Vec<EnvGuard>,
    }

    impl Fixture {
        pub(in crate::cli::commands) fn new(config: &str) -> Self {
            let home = tempdir().unwrap();
            let mut guards = vec![EnvGuard::set(
                "ACTUAL_CONFIG_DIR",
                home.path().to_str().unwrap(),
            )];
            for name in [
                "ACTUAL_CONFIG",
                "ACTUAL_NO_TELEMETRY",
                "ACTUAL_OTLP_LOGS_ENDPOINT",
                "ACTUAL_OTLP_FILE",
                "TRACEPARENT",
            ] {
                guards.push(EnvGuard::remove(name));
            }
            #[cfg(feature = "telemetry")]
            guards.extend(
                [
                    crate::telemetry::debug::ENV,
                    crate::telemetry::debug::ID_ENV,
                ]
                .map(EnvGuard::remove),
            );
            std::fs::write(home.path().join("config.yaml"), config).unwrap();
            let repo = tempdir().unwrap();
            let rules = crate::rules::rules_dir(repo.path());
            std::fs::create_dir_all(&rules).unwrap();
            std::fs::create_dir_all(repo.path().join("src")).unwrap();
            #[cfg(feature = "telemetry")]
            let header = super::super::check_telemetry::tests::with_header;
            // Without telemetry no test asks, so plain rule files will do.
            #[cfg(not(feature = "telemetry"))]
            let header = |_: &str, _: Option<&str>, body: &str| body.to_string();
            for (slug, contents) in [
                (
                    "api-errors-001",
                    header(
                        "adr-errors-1",
                        Some("grp-errors"),
                        &format!("# Typed API Errors: Handlers\n\n{RULES}"),
                    ),
                ),
                (
                    "api-errors-002",
                    header(
                        "adr-errors-2",
                        Some("grp-errors"),
                        &format!("# Typed API Errors: Clients\n\n{RULES}"),
                    ),
                ),
                (
                    "logging-001",
                    header(
                        "adr-logging",
                        None,
                        &format!("# Structured Logging\n\n{RULES}"),
                    ),
                ),
                ("legacy-naming", format!("# Legacy Naming\n\n{RULES}")),
            ] {
                std::fs::write(rules.join(format!("{slug}.md")), contents).unwrap();
            }
            std::fs::write(repo.path().join("src/orders.rs"), "fn a() {}\nfn b() {}\n").unwrap();
            git(repo.path(), &["init", "-q"]);
            git(repo.path(), &["add", "."]);
            git(repo.path(), &["commit", "-q", "-m", "baseline"]);
            Self {
                _home: home,
                repo,
                _guards: guards,
            }
        }

        pub(in crate::cli::commands) fn rules_dir(&self) -> PathBuf {
            crate::rules::rules_dir(self.repo.path())
        }

        /// Record every decision the rule set holds as briefed in `session`.
        pub(in crate::cli::commands) fn brief_all(&self, session: &str) {
            let index = crate::rules::scope::resolve_in(&self.rules_dir(), self.repo.path(), false)
                .unwrap()
                .index;
            let keys: Vec<String> = index
                .documents
                .iter()
                .map(|document| document.decision_key())
                .collect();
            let rules_dir = super::super::rules_brief::resolve(&self.rules_dir());
            brief_ledger::record(
                &brief_ledger::ledger_dir().unwrap(),
                &LedgerKey {
                    session_id: session,
                    rules_dir: &rules_dir,
                },
                keys.iter().map(String::as_str),
            );
        }

        /// Ask `session`'s current turn, returning the token.
        fn ask(&self, session: &str) -> String {
            self.brief_all(session);
            let state = governance_session::load(session, &self.rules_dir());
            let ask = prepare(
                Some(session),
                &state,
                self.repo.path(),
                &self.rules_dir(),
                None,
                "0123456789abcdef",
            )
            .unwrap();
            assert!(mark(session, &self.rules_dir(), &ask, false));
            ask.token
        }

        fn args(&self, session: &str, turn: &str) -> RulesReportUseArgs {
            RulesReportUseArgs {
                claude_session: session.to_string(),
                turn: turn.to_string(),
                claims: Vec::new(),
                plan_claims: Vec::new(),
                none: false,
                repo: Some(self.repo.path().to_path_buf()),
                rules_dir: None,
            }
        }

        fn answer(&self, session: &str) -> Option<Answer> {
            governance_session::load(session, &self.rules_dir())
                .adr_ask
                .and_then(|ask| ask.answer)
        }
    }

    fn git(cwd: &Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .args(args)
            .current_dir(cwd)
            .env("GIT_AUTHOR_NAME", "Test")
            .env("GIT_AUTHOR_EMAIL", "test@example.com")
            .env("GIT_COMMITTER_NAME", "Test")
            .env("GIT_COMMITTER_EMAIL", "test@example.com")
            .stdout(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    }

    fn lock() -> std::sync::MutexGuard<'static, ()> {
        ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn reasons(answer: &Answer) -> Vec<Option<&'static str>> {
        answer
            .claims
            .iter()
            .map(|claim| claim.reason.map(Unverified::as_str))
            .collect()
    }

    /// Telemetry on, with no output configured, so a test build sends nothing.
    const TELEMETRY_ON: &str = "telemetry:\n  enabled: true\n";

    const NO_TELEMETRY: &str = "telemetry:\n  enabled: false\n";

    #[test]
    fn test_changed_lines_reads_each_files_added_lines() {
        let diff = "diff --git a/src/a.rs b/src/a.rs\n--- a/src/a.rs\n+++ b/src/a.rs\n@@ -1,3 +1,4 @@\n fn a() {}\n-fn b() {}\n+fn b() -> Result<(), ApiError> {}\n+++counter;\n fn c() {}\n\\ No newline at end of file\n\
diff --git a/new.rs b/new.rs\nnew file mode 100644\n--- /dev/null\n+++ b/new.rs\n@@ -0,0 +1,2 @@\n+one\n+two\n\
diff --git a/gone.rs b/gone.rs\ndeleted file mode 100644\n--- a/gone.rs\n+++ /dev/null\n@@ -1 +0,0 @@\n-gone\n\
diff --git \"a/sp ace\\303\\251.rs\" \"b/sp ace\\303\\251.rs\"\n--- \"a/sp ace\\303\\251.rs\"\n+++ \"b/sp ace\\303\\251.rs\"\n@@ -1 +1 @@\n-x\n+y\n";
        let changed = changed_lines(diff);
        let lines = |file: &str| {
            changed[file]
                .lines
                .iter()
                .map(|(n, (_, t))| (*n, t.as_str()))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            lines("src/a.rs"),
            [(2, "fn b() -> Result<(), ApiError> {}"), (3, "++counter;")]
        );
        assert_eq!(lines("new.rs"), [(1, "one"), (2, "two")]);
        assert_eq!(lines("sp aceé.rs"), [(1, "y")]);
        assert_eq!(changed["src/a.rs"].hunks, vec![1..5]);
        assert_eq!(changed["new.rs"].hunks, vec![1..3]);
        assert!(!changed.contains_key("gone.rs"), "{changed:?}");
        assert_eq!(changed.len(), 3);
        // A line that is no part of a hunk ends it.
        let stray = changed_lines("+++ b/x.rs\n@@ -1 +1 @@\n+kept\nnot a diff line\n+dropped\n");
        assert_eq!(stray["x.rs"].lines.len(), 1);
        // A file whose hunks only remove lines adds none.
        let removed = changed_lines("+++ b/y.rs\n@@ -1,2 +1 @@\n keep\n-drop\n");
        assert!(removed.is_empty(), "{removed:?}");
        assert_eq!(hunk_start("-1 +x @@"), None);
    }

    /// A claim holds on its own line when that line was added, else on the
    /// nearest added line within three in the same hunk, the earlier on a tie.
    #[test]
    fn test_nearest_prefers_the_exact_line_then_the_closest_in_its_hunk() {
        let diff = "diff --git a/f.rs b/f.rs\n--- a/f.rs\n+++ b/f.rs\n\
@@ -1,10 +1,11 @@\n+a\n 2\n 3\n 4\n 5\n 6\n+b\n 8\n 9\n 10\n 11\n\
@@ -20,2 +12,3 @@\n 12\n+c\n 14\n";
        let changed = changed_lines(diff);
        let file = &changed["f.rs"];
        assert_eq!(file.hunks, [1..12, 12..15]);
        let nearest = |line| file.nearest(line);
        assert_eq!(nearest(7), Some((7, "b")), "exact hit");
        assert_eq!(nearest(9), Some((7, "b")), "near hit, after");
        assert_eq!(nearest(3), Some((1, "a")), "near hit, before");
        assert_eq!(nearest(4), Some((1, "a")), "a tie goes to the earlier line");
        assert_eq!(nearest(10), Some((7, "b")), "three away still holds");
        assert_eq!(
            nearest(11),
            None,
            "too far: four from b, and c is in another hunk"
        );
        assert_eq!(nearest(12), Some((13, "c")), "its own hunk");
        assert_eq!(nearest(16), None, "past every hunk");
        assert_eq!((offset(9, 7), offset(3, 1), offset(12, 13)), (-2, -2, 1));
    }

    #[test]
    fn test_unquote_reads_c_quoted_paths() {
        assert_eq!(unquote("b/plain.rs").as_deref(), Some("b/plain.rs"));
        assert_eq!(unquote("/dev/null"), None);
        assert_eq!(
            unquote(r#""b/\a\b\f\v\r\n\t\"\\\303\251""#).as_deref(),
            Some("b/\u{7}\u{8}\u{c}\u{b}\r\n\t\"\\é")
        );
        assert_eq!(unquote(r#""b/trailing\""#), None);
        assert_eq!(unquote(r#""b/\39""#), None);
        assert_eq!(unquote(r#""b/\777""#), None);
        assert_eq!(unquote(r#""b/\377""#), None, "not UTF-8");
    }

    #[test]
    fn test_shell_word_and_clean() {
        assert_eq!(shell_word("/repo/a-b_c.d"), "/repo/a-b_c.d");
        assert_eq!(shell_word("/my repo/it's"), r"'/my repo/it'\''s'");
        assert_eq!(shell_word(""), "''");
        assert_eq!(clean("  a\tb\n\u{1b}[31m c  ", 100), "a b [31m c");
        assert_eq!(clean("abcdef", 3), "abc");
    }

    #[test]
    fn test_repository_path_is_relative_to_the_checkout() {
        let checkout = tempdir().unwrap();
        let real = checkout.path().canonicalize().unwrap();
        std::fs::create_dir_all(real.join("src")).unwrap();
        let some = |path: &str| Some(path.to_string());
        assert_eq!(repository_path("./src/a.rs", &real), some("src/a.rs"));
        assert_eq!(repository_path("docs/new.md", &real), some("docs/new.md"));
        // The working directory plays no part. Here it sits one level inside
        // the checkout and holds its own `src/`, so a path read from it would
        // come back prefixed with that directory's name.
        let cwd = std::env::current_dir().unwrap().canonicalize().unwrap();
        assert!(cwd.join("src").is_dir(), "{}", cwd.display());
        let around_cwd = cwd.parent().unwrap();
        assert_eq!(repository_path("src/a.rs", around_cwd), some("src/a.rs"));
        assert_eq!(repository_path("./src/a.rs", around_cwd), some("src/a.rs"));
        let absolute = real.join("src/a.rs");
        assert_eq!(
            repository_path(absolute.to_str().unwrap(), &real),
            some("src/a.rs")
        );
        let link_parent = tempdir().unwrap();
        let link = link_parent.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let through_link = link.join("src/a.rs");
        assert_eq!(
            repository_path(through_link.to_str().unwrap(), &real),
            some("src/a.rs")
        );
    }

    /// A path that leaves the checkout, climbs with `..` or runs past the
    /// cap names no repository file.
    #[test]
    fn test_repository_path_rejects_paths_outside_the_checkout() {
        let checkout = tempdir().unwrap();
        let real = checkout.path().canonicalize().unwrap();
        std::fs::create_dir_all(real.join("src")).unwrap();
        let escape = format!("{}/src/../../other/secret.txt", real.display());
        let long = format!("src/{}.rs", "a".repeat(MAX_PATH_BYTES));
        for path in [
            "/Users/me/other/secret.txt",
            "../other/secret.txt",
            "src/../src/a.rs",
            escape.as_str(),
            long.as_str(),
            ".",
            real.to_str().unwrap(),
        ] {
            assert_eq!(repository_path(path, &real), None, "{path}");
        }
        let at_cap = format!("src/{}", "a".repeat(MAX_PATH_BYTES - 4));
        assert_eq!(repository_path(&at_cap, &real), Some(at_cap.clone()));
    }

    #[test]
    fn test_flag_claims_read_diff_and_plan_claims() {
        let mut args = Fixture::args_only();
        args.claims = vec!["api-errors-001=src/a:b.rs:12".to_string()];
        args.plan_claims = vec!["logging-001=3".to_string()];
        let claims = flag_claims(&args).unwrap();
        assert_eq!(claims[0].file.as_deref(), Some("src/a:b.rs"));
        assert_eq!((claims[0].line, claims[0].anchor), (12, Anchor::Diff));
        assert_eq!(
            (claims[1].line, claims[1].anchor, claims[1].file.clone()),
            (3, Anchor::Plan, None)
        );
        for bad in [
            "api-errors-001",
            "api-errors-001=src/a.rs",
            "=src/a.rs:1",
            "x=:1",
            "x=a.rs:0",
            "x=a.rs:y",
        ] {
            args.claims = vec![bad.to_string()];
            args.plan_claims.clear();
            let err = flag_claims(&args)
                .err()
                .map(|e| e.to_string())
                .unwrap_or_default();
            assert!(
                err.contains("--claim <adr>=<repo-relative-path>:<line>"),
                "{bad}: {err}"
            );
        }
        args.claims.clear();
        args.plan_claims = vec!["logging-001".to_string()];
        assert!(flag_claims(&args).is_err());
    }

    #[test]
    fn test_json_claims_read_diff_and_plan_claims() {
        let claims = json_claims(
            r#"[{"adr":" api-errors-001 ","file":"src/a.rs","line":4,"note":" because\nof it "},{"adr":"logging-001","plan_line":2},{"adr":"x","file":"a","line":1,"note":"  "}]"#,
        )
        .unwrap();
        assert_eq!(claims[0].adr, "api-errors-001");
        assert_eq!(claims[0].note.as_deref(), Some("because of it"));
        assert_eq!((claims[1].anchor, claims[1].line), (Anchor::Plan, 2));
        assert_eq!(claims[2].note, None);
        assert!(json_claims("[]").unwrap().is_empty());
        for bad in [
            "{}",
            r#"[{"adr":"x","file":"a.rs"}]"#,
            r#"[{"adr":"x","file":"","line":1}]"#,
            r#"[{"adr":"x","file":"a.rs","line":1,"plan_line":1}]"#,
            r#"[{"adr":"x","plan_line":0}]"#,
            r#"[{"adr":" ","plan_line":1}]"#,
        ] {
            assert!(json_claims(bad).is_err(), "{bad}");
        }
    }

    impl Fixture {
        fn args_only() -> RulesReportUseArgs {
            RulesReportUseArgs {
                claude_session: "s".to_string(),
                turn: "0-x".to_string(),
                claims: Vec::new(),
                plan_claims: Vec::new(),
                none: false,
                repo: None,
                rules_dir: None,
            }
        }
    }

    /// The question lists every briefed decision once, by its first file's
    /// slug and its title, and names the command with this turn filled in.
    #[cfg(feature = "telemetry")]
    #[test]
    fn test_prepare_lists_every_briefed_adr_and_the_command() {
        let _lock = lock();
        let fx = Fixture::new(TELEMETRY_ON);
        fx.brief_all("s1");
        let state = governance_session::load("s1", &fx.rules_dir());
        let ask = prepare(
            Some("s1"),
            &state,
            fx.repo.path(),
            &fx.rules_dir(),
            None,
            "0123456789abcdef",
        )
        .unwrap();
        let text = ask.text();
        assert!(text.starts_with(&format!("{QUESTION_HEAD}\n\n")));
        assert!(text.contains("An ADR counts only if it changed or decided what you wrote."));
        assert!(text.contains("merely complies with or happens to agree with"));
        assert!(text.contains("the line you added or changed this turn that best shows its effect"));
        assert!(text.contains("run the command with --none in place of the claims. That is a normal, expected answer."));
        for line in [
            "- `api-errors-001`: Typed API Errors",
            "- `legacy-naming`: Legacy Naming",
            "- `logging-001`: Structured Logging",
        ] {
            assert!(text.contains(line), "{line}");
        }
        assert!(!text.contains("api-errors-002"));
        let repo = fx.repo.path().to_string_lossy();
        let command = format!("actual rules report-use --claude-session s1 --turn 0-01234567 --repo {} --claim <adr>=<repo-relative-path>:<line>", shell_word(&repo));
        assert!(text.contains(&command));
        assert!(!text.contains("--rules-dir"));
        let errors = ask
            .adrs
            .iter()
            .find(|adr| adr.handle == "api-errors-001")
            .unwrap();
        assert_eq!(errors.adr_ref.as_deref(), Some("grp-errors"));
        assert_eq!(errors.adr_ids, ["adr-errors-1", "adr-errors-2"]);
        let logging = ask
            .adrs
            .iter()
            .find(|adr| adr.handle == "logging-001")
            .unwrap();
        assert_eq!(logging.adr_ref.as_deref(), Some("adr-logging"));
        let legacy = ask
            .adrs
            .iter()
            .find(|adr| adr.handle == "legacy-naming")
            .unwrap();
        assert_eq!((legacy.adr_ref.clone(), legacy.adr_ids.len()), (None, 0));

        let other_rules = fx.repo.path().join("other-rules");
        let text = question("s1", "0-x", fx.repo.path(), &other_rules, &[]);
        assert!(text.contains(&format!(
            " --rules-dir {} --claim",
            shell_word(&other_rules.to_string_lossy())
        )));
    }

    #[cfg(feature = "telemetry")]
    #[test]
    fn test_prepare_asks_nothing_without_a_session_a_brief_or_a_new_turn() {
        let _lock = lock();
        let fx = Fixture::new(TELEMETRY_ON);
        let state = GovernanceSession::default();
        let (root, rules) = (fx.repo.path(), fx.rules_dir());
        assert!(prepare(None, &state, root, &rules, None, "d").is_none());
        assert!(prepare(Some(""), &state, root, &rules, None, "d").is_none());
        assert!(
            prepare(Some("s1"), &state, root, &rules, None, "d").is_none(),
            "nothing briefed"
        );
        let token = fx.ask("s1");
        assert_eq!(token, "0-01234567");
        let state = governance_session::load("s1", &rules);
        assert!(
            prepare(Some("s1"), &state, root, &rules, None, "d").is_none(),
            "asked this turn"
        );
        governance_session::update("s1", &rules, |s| s.turn += 1);
        let state = governance_session::load("s1", &rules);
        let next = prepare(Some("s1"), &state, root, &rules, None, "d").unwrap();
        assert_eq!(next.token, "1-d");
    }

    /// Whether `session`, with every decision briefed, gets a question, and
    /// whether `plan-check` would keep its plan.
    fn asks_and_keeps(fx: &Fixture, session: &str) -> (bool, bool) {
        fx.brief_all(session);
        let state = governance_session::load(session, &fx.rules_dir());
        let asks = prepare(
            Some(session),
            &state,
            fx.repo.path(),
            &fx.rules_dir(),
            None,
            "d",
        )
        .is_some();
        keep_plan(session, &fx.rules_dir(), "# Plan\n");
        let keeps = governance_session::load(session, &fx.rules_dir())
            .turn_plan
            .is_some();
        (asks, keeps)
    }

    /// With telemetry off nothing reads the answer, so nothing is asked and no
    /// plan is kept, whichever opt-out turned it off. Debug mode, which logs
    /// the records it would send, still asks.
    #[cfg(feature = "telemetry")]
    #[test]
    fn test_telemetry_off_asks_nothing_and_keeps_no_plan() {
        let _lock = lock();
        let fx = Fixture::new(TELEMETRY_ON);
        assert_eq!(asks_and_keeps(&fx, "on"), (true, true));
        let fx = Fixture::new(NO_TELEMETRY);
        assert_eq!(asks_and_keeps(&fx, "config"), (false, false));
        let fx = Fixture::new(TELEMETRY_ON);
        let _env = EnvGuard::set("ACTUAL_NO_TELEMETRY", "1");
        assert_eq!(asks_and_keeps(&fx, "env"), (false, false));
        let fx = Fixture::new(NO_TELEMETRY);
        let _debug = EnvGuard::set(crate::telemetry::debug::ENV, "1");
        assert_eq!(asks_and_keeps(&fx, "debug"), (true, true));
    }

    /// A build without the `telemetry` feature reports nothing, so it never
    /// asks, even with telemetry on in the config.
    #[cfg(not(feature = "telemetry"))]
    #[test]
    fn test_a_build_without_telemetry_asks_nothing_and_keeps_no_plan() {
        let _lock = lock();
        let fx = Fixture::new(TELEMETRY_ON);
        assert_eq!(asks_and_keeps(&fx, "s1"), (false, false));
    }

    /// A diff matching the one the turn started with asks nothing only when it
    /// was taken from the turn's recorded commit. Taken from `HEAD` after
    /// that commit stopped being an ancestor, the same digest still asks.
    #[cfg(feature = "telemetry")]
    #[test]
    fn test_prepare_reads_a_diff_as_unchanged_only_from_the_turns_own_base() {
        let _lock = lock();
        let fx = Fixture::new(TELEMETRY_ON);
        let (root, rules) = (fx.repo.path(), fx.rules_dir());
        fx.brief_all("s1");
        let head = "a".repeat(40);
        governance_session::update("s1", &rules, |s| {
            s.turn_start = Some(governance_session::TurnStart {
                root: root.to_path_buf(),
                head: head.clone(),
                diff_digest: Some("d".to_string()),
            });
        });
        let state = governance_session::load("s1", &rules);
        assert!(prepare(Some("s1"), &state, root, &rules, Some(&head), "d").is_none());
        assert!(prepare(Some("s1"), &state, root, &rules, Some(&head), "e").is_some());
        assert!(
            prepare(Some("s1"), &state, root, &rules, None, "d").is_some(),
            "a fallback to HEAD asks"
        );
        let other = "b".repeat(40);
        assert!(prepare(Some("s1"), &state, root, &rules, Some(&other), "d").is_some());
    }

    #[test]
    fn test_asked_adrs_lists_a_decision_the_rule_set_no_longer_holds_by_its_key() {
        let gone = "id:adr-gone".to_string();
        let listed = asked_adrs([&gone].into_iter(), None);
        assert_eq!(listed[0].0.handle, "id:adr-gone");
        assert_eq!(listed[0].1, "id:adr-gone");
        assert_eq!(listed[0].0.adr_ref, None);
    }

    #[cfg(feature = "telemetry")]
    #[test]
    fn test_mark_records_the_question_once_and_only_for_its_turn() {
        let _lock = lock();
        let fx = Fixture::new(TELEMETRY_ON);
        let rules = fx.rules_dir();
        fx.brief_all("s1");
        let state = governance_session::load("s1", &rules);
        let ask = prepare(
            Some("s1"),
            &state,
            fx.repo.path(),
            &rules,
            None,
            "abcdef0123",
        )
        .unwrap();
        governance_session::update("s1", &rules, |s| s.turn = 4);
        assert!(!mark("s1", &rules, &ask, false), "the turn moved on");
        governance_session::update("s1", &rules, |s| s.turn = 0);
        assert!(mark("s1", &rules, &ask, false));
        assert!(!mark("s1", &rules, &ask, false), "already asked");
        let recorded = governance_session::load("s1", &rules).adr_ask.unwrap();
        assert_eq!(recorded.token, "0-abcdef01");
        assert!(!recorded.action_id.is_empty());
    }

    /// Each claim is checked against what the question listed and against the
    /// lines this turn added: in a tracked file and in a new one.
    #[cfg(feature = "telemetry")]
    #[test]
    fn test_answer_checks_each_claim_against_the_turn() {
        let _lock = lock();
        let fx = Fixture::new(TELEMETRY_ON);
        let token = fx.ask("s1");
        std::fs::write(
            fx.repo.path().join("src/orders.rs"),
            "fn a() {}\nfn b() -> Result<(), ApiError> {}\n",
        )
        .unwrap();
        std::fs::write(fx.repo.path().join("src/new.rs"), "fn log() {}\n").unwrap();
        let mut args = fx.args("s1", &token);
        args.claims = [
            "api-errors-001=src/orders.rs:2",
            "adr-errors-2=src/orders.rs:1",
            "logging-001=src/new.rs:1",
            "logging-001=src/untouched.rs:1",
            "made-up-adr=src/orders.rs:2",
            "logging-001=src/orders.rs:9",
        ]
        .map(str::to_string)
        .to_vec();
        let reply = answer(&args, None::<&[u8]>).unwrap();
        assert_eq!(
            reply,
            "Recorded 6 ADR claims for this turn. You can end your turn."
        );
        let recorded = fx.answer("s1").unwrap();
        assert_eq!(
            reasons(&recorded),
            [
                None,
                None,
                None,
                Some("file_not_changed"),
                Some("adr_not_in_session"),
                Some("line_not_in_diff")
            ]
        );
        let changed_b = Some("fn b() -> Result<(), ApiError> {}");
        let exact = &recorded.claims[0];
        assert_eq!(exact.quote.as_deref(), changed_b);
        assert_eq!(exact.file.as_deref(), Some("src/orders.rs"));
        assert_eq!((exact.matched_line, exact.line_offset), (None, None));
        // One line off the changed line: it holds on that line and says so.
        let near = &recorded.claims[1];
        assert_eq!(near.line, 1);
        assert_eq!((near.matched_line, near.line_offset), (Some(2), Some(1)));
        assert_eq!(near.quote.as_deref(), changed_b);
        assert_eq!(recorded.claims[5].quote, None);

        assert_eq!(answer(&args, None::<&[u8]>).unwrap(), ALREADY);
        assert_eq!(
            fx.answer("s1").unwrap(),
            recorded,
            "a second answer changes nothing"
        );
    }

    /// A claim on a path outside the checkout, through `..`, or past the cap
    /// names no file: it is unverified and the session keeps no path for it.
    #[cfg(feature = "telemetry")]
    #[test]
    fn test_answer_keeps_no_path_outside_the_checkout() {
        let _lock = lock();
        let fx = Fixture::new(TELEMETRY_ON);
        let token = fx.ask("s1");
        std::fs::write(
            fx.repo.path().join("src/orders.rs"),
            "fn a() {}\nfn b2() {}\n",
        )
        .unwrap();
        let mut args = fx.args("s1", &token);
        args.claims = vec![
            "api-errors-001=/Users/me/other/secret.txt:1".to_string(),
            "api-errors-001=../other/secret.txt:1".to_string(),
            "api-errors-001=src/../src/orders.rs:2".to_string(),
            format!("api-errors-001=src/{}.rs:2", "a".repeat(MAX_PATH_BYTES)),
        ];
        answer(&args, None::<&[u8]>).unwrap();
        let recorded = fx.answer("s1").unwrap();
        assert_eq!(reasons(&recorded), [Some("file_not_changed"); 4]);
        assert!(recorded.claims.iter().all(|claim| claim.file.is_none()));
    }

    /// A plan claim holds on a non-blank line of the plan judged this turn.
    #[cfg(feature = "telemetry")]
    #[test]
    fn test_answer_checks_plan_claims_against_this_turns_plan() {
        let _lock = lock();
        let fx = Fixture::new(TELEMETRY_ON);
        let rules = fx.rules_dir();
        let token = fx.ask("s1");
        keep_plan("s1", &rules, "# Plan\n\nReturn ApiError from handlers.\n");
        let mut args = fx.args("s1", &token);
        args.plan_claims = ["api-errors-001=3", "api-errors-001=2", "logging-001=9"]
            .map(str::to_string)
            .to_vec();
        answer(&args, None::<&[u8]>).unwrap();
        let recorded = fx.answer("s1").unwrap();
        assert_eq!(
            reasons(&recorded),
            [None, Some("line_not_in_diff"), Some("line_not_in_diff")]
        );
        assert_eq!(
            recorded.claims[0].quote.as_deref(),
            Some("Return ApiError from handlers.")
        );
        // The answer leaves only the plan's line count and the claimed lines.
        let kept = governance_session::load("s1", &rules).turn_plan.unwrap();
        assert_eq!((kept.text, kept.lines), (None, 3));

        // A plan kept in an earlier turn is not this turn's plan.
        let fx = Fixture::new(TELEMETRY_ON);
        let rules = fx.rules_dir();
        keep_plan("s2", &rules, "# Plan\n");
        governance_session::update("s2", &rules, |s| s.turn = 1);
        let token = fx.ask("s2");
        let mut args = fx.args("s2", &token);
        args.plan_claims = vec!["logging-001=1".to_string()];
        answer(&args, None::<&[u8]>).unwrap();
        assert_eq!(
            reasons(&fx.answer("s2").unwrap()),
            [Some("file_not_changed")]
        );
    }

    #[cfg(feature = "telemetry")]
    #[test]
    fn test_answer_reads_json_on_stdin_and_needs_some_answer() {
        let _lock = lock();
        let fx = Fixture::new(TELEMETRY_ON);
        let token = fx.ask("s1");
        let args = fx.args("s1", &token);
        let err = answer(&args, None::<&[u8]>).unwrap_err().to_string();
        assert!(
            err.contains("give --claim, --plan-claim or --none"),
            "{err}"
        );
        assert_eq!(
            answer(&args, Some(&b"[]"[..])).unwrap(),
            "Recorded that no ADR shaped this turn's change. You can end your turn."
        );
        assert!(fx.answer("s1").unwrap().claims.is_empty());

        let token = {
            governance_session::update("s1", &fx.rules_dir(), |s| s.turn += 1);
            fx.ask("s1")
        };
        let mut args = fx.args("s1", &token);
        let json = br#"[{"adr":"logging-001","file":"src/orders.rs","line":1,"note":"kept it"}]"#;
        assert_eq!(
            answer(&args, Some(&json[..])).unwrap(),
            "Recorded 1 ADR claim for this turn. You can end your turn."
        );
        assert_eq!(
            fx.answer("s1").unwrap().claims[0].note.as_deref(),
            Some("kept it")
        );
        args.claims = vec!["x=y:1".to_string(); MAX_CLAIMS + 1];
        assert!(answer(&args, None::<&[u8]>)
            .unwrap_err()
            .to_string()
            .contains("at most 50"));
    }

    #[cfg(feature = "telemetry")]
    #[test]
    fn test_answer_refuses_without_an_open_question() {
        let _lock = lock();
        let fx = Fixture::new(TELEMETRY_ON);
        let mut args = fx.args("s1", "0-01234567");
        args.none = true;
        let err = answer(&args, None::<&[u8]>).unwrap_err().to_string();
        assert!(
            err.contains("no ADR question is open for session s1 at turn 0-01234567"),
            "{err}"
        );
        let token = fx.ask("s1");
        args.turn = "0-ffffffff".to_string();
        assert!(answer(&args, None::<&[u8]>).is_err(), "another token");
        args.turn = token;
        governance_session::update("s1", &fx.rules_dir(), |s| s.turn += 1);
        assert!(answer(&args, None::<&[u8]>).is_err(), "the turn moved on");
        assert!(fx.answer("s1").is_none());
    }

    #[cfg(feature = "telemetry")]
    mod telemetry {
        use super::*;
        use crate::cli::commands::brief_telemetry::tests::{attribute, take_captured};

        const WITH_FILE: &str = "telemetry:\n  otlp_file: /tmp/actual-otlp-report-test.jsonl\n";

        /// One event per ADR named: verified when any of its claims held, with
        /// the asking check's action id, and the file path only when verified,
        /// and one with an empty ref for a name the question never listed. The
        /// ADR without a v2 header sends nothing.
        #[test]
        fn test_report_sends_one_event_per_named_adr() {
            let _lock = lock();
            let fx = Fixture::new(WITH_FILE);
            let token = fx.ask("s1");
            let action_id = governance_session::load("s1", &fx.rules_dir())
                .adr_ask
                .unwrap()
                .action_id;
            std::fs::write(
                fx.repo.path().join("src/orders.rs"),
                "fn a() {}\nfn b2() {}\n",
            )
            .unwrap();
            take_captured();
            let mut args = fx.args("s1", &token);
            args.claims = [
                "api-errors-001=src/orders.rs:1",
                "adr-errors-2=src/orders.rs:2",
                "api-errors-001=src/orders.rs:1",
                "legacy-naming=src/orders.rs:2",
                "made-up=src/orders.rs:2",
                "logging-001=src/orders.rs:9",
            ]
            .map(str::to_string)
            .to_vec();
            args.plan_claims = vec!["logging-001=1".to_string()];
            answer(&args, None::<&[u8]>).unwrap();
            let records = take_captured();
            let summary: Vec<_> = records
                .iter()
                .map(|record| {
                    assert_eq!(record["eventName"], "actual.adr.reported");
                    assert_eq!(attribute(record, "actual.session.id"), "s1");
                    assert_eq!(
                        attribute(record, "actual.report.check_action_id"),
                        action_id.as_str()
                    );
                    assert_eq!(
                        record["attributes"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .find(|kv| kv["key"] == "actual.turn.index")
                            .unwrap()["value"]["intValue"],
                        "0"
                    );
                    let file = record["attributes"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .find(|kv| kv["key"] == "actual.report.file_path")
                        .map(|kv| kv["value"]["stringValue"].clone());
                    (
                        attribute(record, "actual.adr.ref"),
                        attribute(record, "actual.report.evidence"),
                        attribute(record, "actual.report.evidence_reason"),
                        attribute(record, "actual.report.anchor"),
                        file,
                    )
                })
                .collect();
            let s = |v: &str| serde_json::Value::String(v.to_string());
            assert_eq!(
                summary,
                [
                    (
                        s("grp-errors"),
                        s("verified"),
                        s(""),
                        s("diff"),
                        Some(s("src/orders.rs"))
                    ),
                    (
                        s(""),
                        s("unverified"),
                        s("adr_not_in_session"),
                        s("diff"),
                        None
                    ),
                    (
                        s("adr-logging"),
                        s("unverified"),
                        s("line_not_in_diff"),
                        s("diff"),
                        None
                    ),
                ]
            );
            for record in &records {
                let text = record.to_string();
                assert!(
                    !text.contains("fn b2()") && !text.contains("made-up"),
                    "{text}"
                );
            }
        }

        /// Fifty names the question never listed send one event between them,
        /// and no claim that failed its check sends a path, even one outside
        /// the checkout.
        #[test]
        fn test_report_sends_one_event_for_every_unlisted_name() {
            let _lock = lock();
            let fx = Fixture::new(WITH_FILE);
            let token = fx.ask("s1");
            std::fs::write(
                fx.repo.path().join("src/orders.rs"),
                "fn a() {}\nfn b2() {}\n",
            )
            .unwrap();
            take_captured();
            let mut args = fx.args("s1", &token);
            args.claims = (0..MAX_CLAIMS - 2)
                .map(|n| format!("unlisted-{n}=src/orders.rs:2"))
                .chain([
                    "api-errors-001=/Users/me/other/secret.txt:1".to_string(),
                    "unlisted-x=/Users/me/other/secret.txt:1".to_string(),
                ])
                .collect();
            assert_eq!(args.claims.len(), 50);
            answer(&args, None::<&[u8]>).unwrap();
            let records = take_captured();
            assert_eq!(records.len(), 2, "{records:?}");
            let refs: Vec<_> = records
                .iter()
                .map(|record| attribute(record, "actual.adr.ref"))
                .collect();
            assert_eq!(refs, ["", "grp-errors"]);
            for record in &records {
                let text = record.to_string();
                assert!(!text.contains("actual.report.file_path"), "{text}");
                assert!(!text.contains("unlisted") && !text.contains("secret"));
            }
        }

        /// A verified plan claim reports its ADR anchored to the plan, with no
        /// file path to send.
        #[test]
        fn test_report_sends_a_plan_claim_with_no_file_path() {
            let _lock = lock();
            let fx = Fixture::new(WITH_FILE);
            let rules = fx.rules_dir();
            let token = fx.ask("s1");
            keep_plan("s1", &rules, "# Plan\n\nLog every order as JSON.\n");
            take_captured();
            let mut args = fx.args("s1", &token);
            args.plan_claims = vec!["logging-001=3".to_string()];
            answer(&args, None::<&[u8]>).unwrap();
            let records = take_captured();
            assert_eq!(records.len(), 1);
            let record = &records[0];
            assert_eq!(attribute(record, "actual.adr.ref"), "adr-logging");
            assert_eq!(attribute(record, "actual.report.evidence"), "verified");
            assert_eq!(attribute(record, "actual.report.anchor"), "plan");
            assert!(!record["attributes"]
                .as_array()
                .unwrap()
                .iter()
                .any(|kv| kv["key"] == "actual.report.file_path"));
        }

        /// Opting out between the question and the answer sends nothing.
        #[test]
        fn test_report_sends_nothing_with_telemetry_off() {
            let _lock = lock();
            let fx = Fixture::new(WITH_FILE);
            let token = fx.ask("s1");
            let _off = EnvGuard::set("ACTUAL_NO_TELEMETRY", "1");
            take_captured();
            let mut args = fx.args("s1", &token);
            args.claims = vec!["api-errors-001=src/orders.rs:1".to_string()];
            answer(&args, None::<&[u8]>).unwrap();
            assert!(take_captured().is_empty());
        }
    }
}
