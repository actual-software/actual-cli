//! Every file under `docs/adr/` and `.actual/rules/` sorted into a state, for
//! `actual adr discover` and `actual adr normalize`.
//!
//! # Design
//!
//! Three kinds of decision docs share `docs/adr/`:
//!
//! * Actual AI's repository sync writes a doc and a rule file for each
//!   decision. Both carry a header with the decision's `adr_id`, and the rule
//!   file has no source record ([`crate::adr::provenance`]).
//! * This CLI writes the same pair (`adr new`, `adr normalize`), and its rule
//!   file records the doc it came from and a digest of what it governs. A
//!   doc whose digest no longer matches has been edited since, and its rule
//!   file is stale.
//! * Anything else — a hand-written ADR, one an agent wrote, an index — has
//!   no `adr_id` header at all and governs nothing until it is normalized.
//!
//! `actual adr-bot` also writes ADRs in an older layout, with a
//! `.claude/rules/` file beside each. Those are reported as [`State::Legacy`]
//! and left alone: adr-bot writes them again on its next run, and their ids
//! are decisions in Actual AI already.
//!
//! Classification reads files only. Comparing a synced doc with Actual AI and
//! asking git about uncommitted edits happen in the command, on top of this.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;
use serde::Serialize;

use crate::adr::context_file::{self, h1_title, ContextFile};
use crate::adr::files::{self, DecisionFile, DECISION_DIR, RULES_DIR};
use crate::adr::overlap::{find_overlaps, ExistingDecision};
use crate::adr::provenance::digest;
use crate::adr::save_check::check_document;

/// Where adr-bot writes the rule file beside each of its ADRs.
pub const LEGACY_RULES_DIR: &str = ".claude/rules";

/// A comment that keeps a file out of discovery.
pub const IGNORE_MARKER: &str = "<!-- actual:ignore -->";

/// File names that are an ADR directory's furniture rather than decisions,
/// compared without case.
const IGNORED_NAMES: &[&str] = &[
    "readme.md",
    "index.md",
    "toc.md",
    "summary.md",
    "template.md",
    "adr-template.md",
    "_template.md",
];

/// A UUID at the start of a file stem, as the sync and adr-bot name files.
static UUID_PREFIX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^([0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12})")
        .expect("valid regex")
});

/// The id in an adr-bot rule file's `<rule_activation id="…">` or
/// `adr-id="…"`.
static ACTIVATION_ID: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"<rule_activation[^>]*?\b(?:adr-id|id)="([^"]+)""#).expect("valid regex")
});

/// What a file under `docs/adr/` or `.actual/rules/` is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum State {
    /// Written by Actual AI's sync: nothing to do here.
    Synced,
    /// Written by the sync and changed since: the next sync overwrites the
    /// change. Set by the command, which compares with Actual AI.
    SyncedEdited,
    /// Written by this CLI, and its rule file matches the doc.
    Managed,
    /// Written by this CLI, and the doc has changed since its rule file was:
    /// re-index.
    Stale,
    /// The doc has a decision header but no rule file: re-index.
    MissingRule,
    /// The doc has a decision header (or had one) but no longer reads as a
    /// decision: re-normalize.
    Broken,
    /// A decision doc in another format: normalize.
    Unmanaged,
    /// Like `Unmanaged`, but it looks like a decision this repository has
    /// already: review before normalizing.
    PossibleDuplicate,
    /// Written by `actual adr-bot` in its older layout: reported only.
    Legacy,
    /// A rule file whose decision has no doc under `docs/adr/`.
    OrphanRule,
    /// An index, a template, or a file marked to be ignored.
    Ignored,
    /// A file that couldn't be read.
    Unreadable,
}

impl State {
    /// What `actual adr normalize` does for a file in this state.
    pub fn action(self) -> Action {
        match self {
            State::Stale | State::MissingRule => Action::Reindex,
            State::Broken => Action::Renormalize,
            State::Unmanaged => Action::Normalize,
            State::PossibleDuplicate => Action::Review,
            State::SyncedEdited => Action::Push,
            State::Synced
            | State::Managed
            | State::Legacy
            | State::OrphanRule
            | State::Ignored
            | State::Unreadable => Action::None,
        }
    }

    /// The state as `--json` spells it.
    pub fn as_str(self) -> &'static str {
        match self {
            State::Synced => "synced",
            State::SyncedEdited => "synced-edited",
            State::Managed => "managed",
            State::Stale => "stale",
            State::MissingRule => "missing-rule",
            State::Broken => "broken",
            State::Unmanaged => "unmanaged",
            State::PossibleDuplicate => "possible-duplicate",
            State::Legacy => "legacy",
            State::OrphanRule => "orphan-rule",
            State::Ignored => "ignored",
            State::Unreadable => "unreadable",
        }
    }
}

/// What a file needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Action {
    None,
    /// Write the rule file again from the doc.
    Reindex,
    /// Convert the doc again, keeping its id.
    Renormalize,
    /// Convert the doc to a Context File and write its rule file.
    Normalize,
    /// Decide whether it duplicates a decision before normalizing it.
    Review,
    /// Save the edit to Actual AI (`actual adr update`) or revert it.
    Push,
}

impl Action {
    /// Whether `normalize --all` acts on a file that needs this.
    pub fn normalizes(self) -> bool {
        matches!(
            self,
            Action::Reindex | Action::Renormalize | Action::Normalize | Action::Review
        )
    }
}

/// One file and its state.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Entry {
    /// The decision doc, or the rule file for an orphan rule.
    pub path: PathBuf,
    pub state: State,
    pub action: Action,
    /// The decision id the header names, or the one it is paired by.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub adr_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// The paired rule file under `.actual/rules/`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rule_file: Option<PathBuf>,
    /// Why the file is in its state, for a person.
    pub reason: String,
    /// The file this one looks like, for a possible duplicate.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duplicate_of: Option<String>,
    /// adr-bot's files beside a legacy ADR, left as they are.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub legacy_files: Vec<PathBuf>,
    /// The decision the doc reads as, when it reads as one.
    #[serde(skip)]
    pub doc: Option<ContextFile>,
    /// The rule file's `candidate_group_id`, for a synced decision Actual AI
    /// grouped.
    #[serde(skip)]
    pub group_id: Option<String>,
}

impl Entry {
    fn new(path: PathBuf, state: State, reason: impl Into<String>) -> Self {
        Entry {
            path,
            state,
            action: state.action(),
            adr_id: None,
            title: None,
            rule_file: None,
            reason: reason.into(),
            duplicate_of: None,
            legacy_files: Vec::new(),
            doc: None,
            group_id: None,
        }
    }

    /// Move the entry to `state`, with its action and a new reason.
    pub fn set_state(&mut self, state: State, reason: impl Into<String>) {
        self.state = state;
        self.action = state.action();
        self.reason = reason.into();
    }
}

/// `path` relative to `root` with `/` separators, as a source record and the
/// output name it.
pub fn relative(root: &Path, path: &Path) -> String {
    let rel = path.strip_prefix(root).unwrap_or(path);
    rel.to_string_lossy().replace('\\', "/")
}

/// Sort every decision doc under `<root>/docs/adr/` and every rule file
/// under `<root>/.actual/rules/` into a state. Docs come first, by path,
/// then orphan rule files.
pub fn discover(root: &Path) -> Vec<Entry> {
    let rules = files::rule_files(root);
    let mut paired: HashSet<String> = HashSet::new();
    let mut entries: Vec<Entry> = files::decision_docs(root)
        .into_iter()
        .map(|read| match read {
            Ok(file) => classify(root, &file, &rules, &mut paired),
            Err((path, reason)) => Entry::new(path, State::Unreadable, reason),
        })
        .collect();
    mark_duplicates(root, &mut entries);
    for rule in &rules {
        let Some(id) = rule.adr_id() else { continue };
        if paired.contains(id) {
            continue;
        }
        let mut entry = Entry::new(
            rule.path.clone(),
            State::OrphanRule,
            format!("No decision doc under {DECISION_DIR}/ names this rule file's decision."),
        );
        entry.adr_id = Some(id.to_string());
        entries.push(entry);
    }
    entries
}

/// Whether a doc is furniture or marked to be ignored.
fn ignored(file: &DecisionFile) -> bool {
    let name = file
        .path
        .file_name()
        .map(|name| name.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    IGNORED_NAMES.contains(&name.as_str())
        || name.starts_with("0000")
        || file.body.contains(IGNORE_MARKER)
}

/// The UUID a file stem starts with, lowercased.
fn uuid_prefix(path: &Path) -> Option<String> {
    let stem = path.file_stem()?.to_string_lossy();
    UUID_PREFIX
        .captures(&stem)
        .map(|found| found[1].to_ascii_lowercase())
}

fn classify(
    root: &Path,
    file: &DecisionFile,
    rules: &[DecisionFile],
    paired: &mut HashSet<String>,
) -> Entry {
    if ignored(file) {
        return Entry::new(
            file.path.clone(),
            State::Ignored,
            format!("An index, a template, or marked {IGNORE_MARKER}."),
        );
    }
    let rule_for = |id: &str| rules.iter().find(|rule| rule.adr_id() == Some(id));
    if let Some(id) = file.adr_id() {
        paired.insert(id.to_string());
        return classify_headed(root, file, id, rule_for(id));
    }
    if let Some(entry) = legacy(root, file) {
        if let Some(id) = &entry.adr_id {
            paired.insert(id.clone());
        }
        return entry;
    }
    let parsed = context_file::parse(&file.body)
        .ok()
        .map(|parsed| parsed.doc);
    let title = parsed
        .as_ref()
        .map(|doc| doc.title.clone())
        .or_else(|| h1_title(&file.body).map(str::to_string));
    // A doc that lost its header, paired with its rule file by the id in its
    // name, or by the source record of a rule file this CLI wrote from it.
    let rel = relative(root, &file.path);
    let by_name = uuid_prefix(&file.path).and_then(|id| Some((id.clone(), rule_for(&id)?)));
    let by_source = || {
        rules.iter().find_map(|rule| {
            let source = rule.source.as_ref()?;
            (source.is_local() && source.path == rel)
                .then(|| Some((rule.adr_id()?.to_string(), rule)))
                .flatten()
        })
    };
    if let Some((id, rule)) = by_name.or_else(by_source) {
        paired.insert(id.clone());
        let mut entry = Entry::new(
            file.path.clone(),
            State::Broken,
            "Its decision header is gone; its rule file still names the decision.",
        );
        entry.adr_id = Some(id);
        entry.rule_file = Some(rule.path.clone());
        entry.title = title;
        return entry;
    }
    let mut entry = Entry::new(
        file.path.clone(),
        State::Unmanaged,
        "No decision header: written outside Actual, so nothing enforces it yet.",
    );
    entry.title = title;
    entry.doc = parsed;
    entry
}

fn classify_headed(
    root: &Path,
    file: &DecisionFile,
    id: &str,
    rule: Option<&DecisionFile>,
) -> Entry {
    let meta = file.meta.clone().unwrap_or_default();
    let parsed = context_file::parse(&file.body).map(|parsed| parsed.doc);
    let mut entry = Entry::new(file.path.clone(), State::Synced, "");
    entry.adr_id = Some(id.to_string());
    entry.title = parsed.as_ref().ok().map(|doc| doc.title.clone());
    entry.rule_file = rule.map(|rule| rule.path.clone());
    entry.group_id = rule.and_then(|rule| rule.meta.as_ref()?.candidate_group_id.clone());

    // Actual AI's sync writes a doc only with its rule file, drafts included
    // (it writes neither), so a doc without one is never the sync's.
    let Some(rule) = rule else {
        let reason = if meta.is_active() {
            format!("No rule file under {RULES_DIR}/ names this decision, so no check reads it.")
        } else {
            format!("No rule file under {RULES_DIR}/ names this decision.")
        };
        entry.set_state(State::MissingRule, reason);
        entry.doc = parsed.ok();
        return entry;
    };
    let Some(source) = rule.source.as_ref().filter(|source| source.is_local()) else {
        entry.set_state(State::Synced, "Written by Actual AI's sync.");
        entry.doc = parsed.ok();
        return entry;
    };

    let doc = match parsed {
        Ok(doc) => doc,
        Err(reason) => {
            entry.set_state(State::Broken, reason);
            return entry;
        }
    };
    let status = meta
        .status
        .clone()
        .unwrap_or_else(|| "accepted".to_string());
    let as_draft = !status.trim().eq_ignore_ascii_case("accepted");
    if let Err(reasons) = check_document(&doc, as_draft, "1970-01-01") {
        entry.set_state(State::Broken, reasons.join(" "));
        return entry;
    }
    let root_dir = meta.root.clone().unwrap_or_else(|| ".".to_string());
    let current = digest(&doc, &status, &root_dir, &meta.paths);
    let path = relative(root, &file.path);
    if current != source.digest {
        entry.set_state(
            State::Stale,
            "The doc's title, status, scope or policies changed since its rule file was written.",
        );
    } else if path != source.path {
        entry.set_state(
            State::Stale,
            format!(
                "The doc moved since its rule file was written (from {}).",
                source.path
            ),
        );
    } else {
        entry.set_state(State::Managed, "Written by this CLI and up to date.");
    }
    entry.doc = Some(doc);
    entry
}

/// adr-bot's layout: a doc with no header, and a `.claude/rules/` file
/// beside it that opens `<rule_activation`, named after the doc or after the
/// UUID its name starts with.
fn legacy(root: &Path, file: &DecisionFile) -> Option<Entry> {
    let stem = file.path.file_stem()?.to_string_lossy().to_string();
    let dir = root.join(LEGACY_RULES_DIR);
    let mut names = vec![stem];
    names.extend(uuid_prefix(&file.path));
    let (rule_path, text) = names.iter().find_map(|name| {
        let path = dir.join(format!("{name}.md"));
        let text = std::fs::read_to_string(&path).ok()?;
        text.contains("<rule_activation").then_some((path, text))
    })?;
    let id = ACTIVATION_ID
        .captures(&text)
        .map(|found| found[1].to_string())
        .or_else(|| uuid_prefix(&file.path));
    let mut entry = Entry::new(
        file.path.clone(),
        State::Legacy,
        "Written by `actual adr-bot`, which writes it again on its next run. Actual AI's \
         sync replaces it; `actual adr normalize --legacy` converts it in place.",
    );
    entry.adr_id = id;
    entry.title = h1_title(&file.body).map(str::to_string);
    entry.legacy_files = vec![rule_path];
    Some(entry)
}

/// Mark each unmanaged doc that overlaps a decision doc already governing
/// (synced, managed or stale) as a possible duplicate.
fn mark_duplicates(root: &Path, entries: &mut [Entry]) {
    let existing: Vec<ExistingDecision> = entries
        .iter()
        .filter(|entry| matches!(entry.state, State::Synced | State::Managed | State::Stale))
        .filter_map(|entry| {
            let doc = entry.doc.as_ref()?;
            Some(ExistingDecision {
                id: relative(root, &entry.path),
                title: doc.title.clone(),
                policies: doc.policies.iter().map(|p| p.statement.clone()).collect(),
            })
        })
        .collect();
    if existing.is_empty() {
        return;
    }
    for entry in entries
        .iter_mut()
        .filter(|entry| entry.state == State::Unmanaged)
    {
        let Some(title) = entry.title.clone() else {
            continue;
        };
        let policies: Vec<String> = entry
            .doc
            .as_ref()
            .map(|doc| doc.policies.iter().map(|p| p.statement.clone()).collect())
            .unwrap_or_default();
        if let Some(found) = find_overlaps(&title, &policies, &existing)
            .into_iter()
            .next()
        {
            entry.set_state(
                State::PossibleDuplicate,
                format!("It looks like {} (\"{}\").", found.adr_id, found.title),
            );
            entry.duplicate_of = Some(found.adr_id);
        }
    }
}

/// How many entries are in each state, in the order [`State`] lists them.
pub fn counts(entries: &[Entry]) -> Vec<(State, usize)> {
    let mut counts: HashMap<State, usize> = HashMap::new();
    for entry in entries {
        *counts.entry(entry.state).or_default() += 1;
    }
    let mut counts: Vec<(State, usize)> = counts.into_iter().collect();
    counts.sort();
    counts
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adr::context_file::Policy;
    use crate::adr::header::{render_header, AdrHeader};
    use crate::adr::pair::Decision;
    use crate::rules::types::RuleLevel;
    use tempfile::tempdir;

    const ID: &str = "0b6f2a1e-1111-4222-8333-444455556666";
    const OTHER: &str = "c357aaaa-bbbb-4ccc-8ddd-eeeeffff0000";

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn doc(title: &str, statement: &str) -> ContextFile {
        ContextFile {
            title: title.to_string(),
            status: "accepted".to_string(),
            date: "2026-11-19".to_string(),
            context: vec!["Why.".to_string()],
            policies: vec![Policy {
                level: RuleLevel::Must,
                statement: statement.to_string(),
            }],
            ..ContextFile::default()
        }
    }

    /// A pair as `adr new` writes it, with the doc at `docs/adr/<name>`.
    fn local_pair(root: &Path, name: &str, id: &str, doc: &ContextFile) -> (PathBuf, PathBuf) {
        let decision = Decision {
            doc,
            adr_id: id,
            status: "accepted",
            scope: &[],
        };
        let doc_path = root.join(DECISION_DIR).join(name);
        let rule_path = root.join(RULES_DIR).join(format!("rule-{name}"));
        write(&doc_path, &decision.doc_text(name, &[]));
        write(
            &rule_path,
            &decision.rule_text(&format!("rule-{name}"), &format!("docs/adr/{name}")),
        );
        (doc_path, rule_path)
    }

    fn header(id: &str, status: &str, group: bool) -> String {
        let mut header = render_header(&AdrHeader {
            name: "x".to_string(),
            description: "x".to_string(),
            adr_id: id.to_string(),
            status: status.to_string(),
            scope_root: ".".to_string(),
            scope_paths: Vec::new(),
            source: None,
        });
        if group {
            header = header.replace(
                "  scope:\n",
                "    candidate_group_id: 34467253-1dfe-5f7a-9f93-241807d9852a\n  scope:\n",
            );
        }
        header
    }

    /// A pair as the sync writes it.
    fn synced_pair(root: &Path, id: &str, doc: &ContextFile, status: &str) -> PathBuf {
        let doc_path = root.join(DECISION_DIR).join(format!("{id}-synced.md"));
        write(
            &doc_path,
            &format!(
                "{}\n{}",
                header(id, status, false),
                context_file::render(doc)
            ),
        );
        write(
            &root.join(RULES_DIR).join("synced.md"),
            &format!("{}\n# Rule\n\n### Verify\n", header(id, status, true)),
        );
        doc_path
    }

    fn find<'a>(entries: &'a [Entry], name: &str) -> &'a Entry {
        entries
            .iter()
            .find(|entry| entry.path.ends_with(name))
            .unwrap_or_else(|| panic!("no entry for {name}: {entries:#?}"))
    }

    #[test]
    fn an_empty_repository_has_nothing() {
        let dir = tempdir().unwrap();
        assert!(discover(dir.path()).is_empty());
    }

    #[test]
    fn a_pair_this_cli_wrote_is_managed_until_its_doc_changes() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        let (doc_path, rule_path) = local_pair(
            root,
            "a.md",
            ID,
            &doc("Use Postgres", "Store data in Postgres."),
        );
        let entries = discover(root);
        let entry = find(&entries, "a.md");
        assert_eq!(entry.state, State::Managed, "{entry:#?}");
        assert_eq!(entry.action, Action::None);
        assert_eq!(entry.rule_file.as_deref(), Some(rule_path.as_path()));
        assert_eq!(entry.title.as_deref(), Some("Use Postgres"));

        // Prose edits don't make it stale.
        let text = std::fs::read_to_string(&doc_path).unwrap();
        write(&doc_path, &text.replace("- Why.", "- A better why."));
        assert_eq!(find(&discover(root), "a.md").state, State::Managed);

        // A policy edit does.
        write(
            &doc_path,
            &text.replace("Store data in Postgres.", "Store data in MySQL."),
        );
        let entries = discover(root);
        let entry = find(&entries, "a.md");
        assert_eq!(entry.state, State::Stale);
        assert_eq!(entry.action, Action::Reindex);
        assert!(entry.action.normalizes());
    }

    #[test]
    fn a_moved_doc_is_stale() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        let (doc_path, _) = local_pair(
            root,
            "a.md",
            ID,
            &doc("Use Postgres", "Store data in Postgres."),
        );
        std::fs::rename(&doc_path, root.join(DECISION_DIR).join("b.md")).unwrap();
        let entries = discover(root);
        let entry = find(&entries, "b.md");
        assert_eq!(entry.state, State::Stale);
        assert!(entry.reason.contains("moved"), "{}", entry.reason);
    }

    #[test]
    fn a_headed_doc_that_no_longer_parses_or_checks_is_broken() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        let (doc_path, _) = local_pair(
            root,
            "a.md",
            ID,
            &doc("Use Postgres", "Store data in Postgres."),
        );
        let text = std::fs::read_to_string(&doc_path).unwrap();
        write(&doc_path, &text.replace("## Decision", "## Outcome"));
        assert_eq!(find(&discover(root), "a.md").state, State::Broken);
        assert_eq!(State::Broken.action(), Action::Renormalize);

        // Parses, but the save check refuses a Context File over its size cap.
        write(
            &doc_path,
            &text.replace("- Why.", &format!("- {}", "x".repeat(110_000))),
        );
        let entries = discover(root);
        assert_eq!(find(&entries, "a.md").state, State::Broken);
    }

    #[test]
    fn a_synced_pair_is_synced_and_carries_its_group() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        synced_pair(
            root,
            ID,
            &doc("Use Postgres", "Store data in Postgres."),
            "accepted",
        );
        let entries = discover(root);
        let entry = find(&entries, &format!("{ID}-synced.md"));
        assert_eq!(entry.state, State::Synced);
        assert_eq!(entry.action, Action::None);
        assert!(entry.group_id.is_some());
        assert!(entry.doc.is_some());
    }

    #[test]
    fn a_headed_doc_without_a_rule_file_misses_one_whatever_its_status() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        let body = context_file::render(&doc("Use Postgres", "Store data in Postgres."));
        write(
            &root.join(DECISION_DIR).join("active.md"),
            &format!("{}\n{body}", header(ID, "accepted", false)),
        );
        write(
            &root.join(DECISION_DIR).join("drafts/draft.md"),
            &format!("{}\n{body}", header(OTHER, "draft", false)),
        );
        let entries = discover(root);
        let active = find(&entries, "active.md");
        assert_eq!(active.state, State::MissingRule);
        assert_eq!(active.action, Action::Reindex);
        assert!(active.reason.ends_with("so no check reads it."));
        // `adr new` and `adr normalize` write a draft's rule file too, and the
        // sync writes neither file for a draft, so a draft without one lost it.
        let draft = find(&entries, "draft.md");
        assert_eq!(draft.state, State::MissingRule);
        assert_eq!(draft.action, Action::Reindex);
        assert_eq!(
            draft.reason,
            format!("No rule file under {RULES_DIR}/ names this decision.")
        );
    }

    #[test]
    fn a_doc_without_a_header_is_unmanaged_or_a_duplicate() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        synced_pair(
            root,
            ID,
            &doc(
                "Use Postgres for storage",
                "Store relational data in Postgres.",
            ),
            "accepted",
        );
        write(
            &root.join(DECISION_DIR).join("0007-use-postgres.md"),
            "# Use Postgres for storage\n\n## Decision\n\nWe store relational data in Postgres.\n",
        );
        write(
            &root.join(DECISION_DIR).join("0008-logging.md"),
            "# Structured logging\n\n## Decision\n\n1. MUST: Log JSON lines.\n",
        );
        write(
            &root.join(DECISION_DIR).join("notes.md"),
            "Just some notes.\n",
        );
        let entries = discover(root);
        let dup = find(&entries, "0007-use-postgres.md");
        assert_eq!(dup.state, State::PossibleDuplicate, "{dup:#?}");
        assert_eq!(dup.action, Action::Review);
        assert_eq!(
            dup.duplicate_of.as_deref(),
            Some(format!("docs/adr/{ID}-synced.md").as_str())
        );
        let logging = find(&entries, "0008-logging.md");
        assert_eq!(logging.state, State::Unmanaged);
        assert_eq!(logging.action, Action::Normalize);
        assert_eq!(logging.title.as_deref(), Some("Structured logging"));
        let notes = find(&entries, "notes.md");
        assert_eq!(notes.state, State::Unmanaged);
        assert_eq!(notes.title, None);
    }

    #[test]
    fn furniture_and_marked_files_are_ignored() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        for name in ["README.md", "0000-template.md", "index.md"] {
            write(&root.join(DECISION_DIR).join(name), "# Index\n");
        }
        write(
            &root.join(DECISION_DIR).join("skip.md"),
            &format!("# Skip\n{IGNORE_MARKER}\n"),
        );
        let entries = discover(root);
        assert_eq!(entries.len(), 4);
        assert!(entries.iter().all(|entry| entry.state == State::Ignored));
        assert_eq!(State::Ignored.action(), Action::None);
    }

    #[test]
    fn adr_bot_files_are_legacy_by_name_or_by_id() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        let by_id = format!("{ID}-adopt-modular-libraries.md");
        write(
            &root.join(DECISION_DIR).join(&by_id),
            "The codebase needs modules.\n## Policies\n- Implement modules.\n",
        );
        write(
            &root.join(LEGACY_RULES_DIR).join(format!("{ID}.md")),
            &format!("<rule_activation id=\"{ID}\" title=\"Adopt\" applies_to=\"**/*\">\n"),
        );
        write(
            &root.join(DECISION_DIR).join("use-postgres.md"),
            "# Use Postgres\n\n## Policies\n\n- Use Postgres\n",
        );
        write(
            &root.join(LEGACY_RULES_DIR).join("use-postgres.md"),
            &format!("---\nglob: \"**/*\"\n---\n\n<rule_activation adr-id=\"{OTHER}\">\n"),
        );
        // A `.claude/rules/` file that isn't adr-bot's doesn't count.
        write(&root.join(DECISION_DIR).join("plain.md"), "# Plain\n");
        write(&root.join(LEGACY_RULES_DIR).join("plain.md"), "Be nice.\n");

        let entries = discover(root);
        let first = find(&entries, &by_id);
        assert_eq!(first.state, State::Legacy);
        assert_eq!(first.adr_id.as_deref(), Some(ID));
        assert_eq!(
            first.legacy_files,
            vec![root.join(LEGACY_RULES_DIR).join(format!("{ID}.md"))]
        );
        assert_eq!(first.action, Action::None);
        let second = find(&entries, "use-postgres.md");
        assert_eq!(second.state, State::Legacy);
        assert_eq!(second.adr_id.as_deref(), Some(OTHER));
        assert_eq!(second.title.as_deref(), Some("Use Postgres"));
        assert_eq!(find(&entries, "plain.md").state, State::Unmanaged);
    }

    #[test]
    fn a_legacy_rule_without_an_id_falls_back_to_the_file_name() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        let name = format!("{ID}-x.md");
        write(&root.join(DECISION_DIR).join(&name), "Text.\n");
        write(
            &root.join(LEGACY_RULES_DIR).join(format!("{ID}.md")),
            "<rule_activation>\n",
        );
        let entries = discover(root);
        assert_eq!(find(&entries, &name).adr_id.as_deref(), Some(ID));
    }

    #[test]
    fn a_doc_that_lost_its_header_pairs_by_name_and_orphans_are_reported() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        let (doc_path, rule_path) = local_pair(
            root,
            &format!("{ID}-use-postgres.md"),
            ID,
            &doc("Use Postgres", "Store data in Postgres."),
        );
        let text = std::fs::read_to_string(&doc_path).unwrap();
        let (_, body) = files::split_header(&text).unwrap();
        write(&doc_path, body);
        local_pair(root, "gone.md", OTHER, &doc("Gone", "Do a thing."));
        std::fs::remove_file(root.join(DECISION_DIR).join("gone.md")).unwrap();

        let entries = discover(root);
        let broken = find(&entries, &format!("{ID}-use-postgres.md"));
        assert_eq!(broken.state, State::Broken);
        assert_eq!(broken.adr_id.as_deref(), Some(ID));
        assert_eq!(broken.rule_file.as_deref(), Some(rule_path.as_path()));
        assert_eq!(broken.title.as_deref(), Some("Use Postgres"));
        // Without an id in its name, the rule file's source record pairs it.
        let (plain, _) = local_pair(
            root,
            "plain.md",
            "a1b2c3d4-0000-4000-8000-000000000001",
            &doc("Plain", "Do it."),
        );
        let text = std::fs::read_to_string(&plain).unwrap();
        write(&plain, files::split_header(&text).unwrap().1);
        let entries = discover(root);
        let paired = find(&entries, "plain.md");
        assert_eq!(paired.state, State::Broken);
        assert_eq!(
            paired.adr_id.as_deref(),
            Some("a1b2c3d4-0000-4000-8000-000000000001")
        );
        assert!(!entries.iter().any(|e| e.path.ends_with("rule-plain.md")));
        let orphan = find(&entries, "rule-gone.md");
        assert_eq!(orphan.state, State::OrphanRule);
        assert_eq!(orphan.adr_id.as_deref(), Some(OTHER));
        // Rule files without an id are not decisions at all, and a synced
        // rule file has no source to pair by.
        write(&root.join(RULES_DIR).join("v1.md"), "# A v1 rule file\n");
        write(
            &root.join(RULES_DIR).join("sourceless.md"),
            &format!(
                "{}\n# R\n",
                header("dddddddd-0000-4000-8000-000000000002", "accepted", false)
            ),
        );
        assert_eq!(discover(root).len(), entries.len() + 1);
    }

    #[test]
    fn unreadable_docs_are_reported() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        let big = root.join(DECISION_DIR).join("big.md");
        write(&big, &"x".repeat(files::MAX_FILE_SIZE as usize + 1));
        let entries = discover(root);
        assert_eq!(find(&entries, "big.md").state, State::Unreadable);
    }

    #[test]
    fn states_spell_their_names_and_count() {
        for (state, name) in [
            (State::Synced, "synced"),
            (State::SyncedEdited, "synced-edited"),
            (State::Managed, "managed"),
            (State::Stale, "stale"),
            (State::MissingRule, "missing-rule"),
            (State::Broken, "broken"),
            (State::Unmanaged, "unmanaged"),
            (State::PossibleDuplicate, "possible-duplicate"),
            (State::Legacy, "legacy"),
            (State::OrphanRule, "orphan-rule"),
            (State::Ignored, "ignored"),
            (State::Unreadable, "unreadable"),
        ] {
            assert_eq!(state.as_str(), name);
            assert_eq!(serde_json::to_value(state).unwrap(), name);
        }
        assert_eq!(State::SyncedEdited.action(), Action::Push);
        assert!(!Action::Push.normalizes());
        let mut a = Entry::new(PathBuf::from("a"), State::Synced, "");
        let b = Entry::new(PathBuf::from("b"), State::Unmanaged, "");
        a.set_state(State::Unmanaged, "now unmanaged");
        assert_eq!(a.action, Action::Normalize);
        assert_eq!(
            counts(&[a, b, Entry::new(PathBuf::from("c"), State::Synced, "")]),
            vec![(State::Synced, 1), (State::Unmanaged, 2)]
        );
    }

    #[test]
    fn relative_paths_use_slashes() {
        assert_eq!(
            relative(Path::new("/r"), Path::new("/r/docs/adr/a.md")),
            "docs/adr/a.md"
        );
        assert_eq!(relative(Path::new("/r"), Path::new("/x/a.md")), "/x/a.md");
    }
}
