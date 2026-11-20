//! The rule files `actual adr refresh` keeps in step with a repository's
//! decisions, and the decision files under `docs/adr/` it can't read.
//!
//! Signed out, the decisions are the files under `docs/adr/`; signed in, they
//! are the repository's decisions in Actual AI. A governing decision gets a
//! rule file under `.actual/rules/`, written from the CLI's template
//! ([`render_rule_file`]) under the name and header the repository sync uses,
//! and a decision that stops governing loses it.
//!
//! The sync has a model write its rule text, so the template never matches a
//! file the sync wrote. A rule file is therefore rewritten only when its
//! decision changed after the file was written. The [`Record`] keeps, per
//! decision, the [`fingerprint`] the decision had when the refresh last wrote
//! its rule file or first met it, and a fingerprint that moved means the
//! decision changed. A rule file the record doesn't know is recorded as it
//! stands.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::adr::context_file::Policy;
use crate::adr::files::{self, DecisionFile, DECISION_DIR, RULES_DIR};
use crate::adr::header::{description_for, render_header, AdrHeader, REPOSITORY_ROOT};
use crate::adr::names::rule_file_name;
use crate::adr::rule_body::{render_rule_body, rule_id_prefix, rule_spelling, scope_line};
use crate::adr::save_check::check_markdown;

/// The header version a normalized decision file carries.
const HEADER_VERSION: &str = "2";

/// The reason a decision file with no ADR header isn't normalized.
pub const NO_HEADER: &str = "It has no ADR header.";

/// The record format this build reads and writes.
pub const RECORD_FORMAT: u32 = 1;

// ── rule files ─────────────────────────────────────────────────────────────

/// What a decision's rule file is written from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleSource {
    pub adr_id: String,
    pub title: String,
    /// The status the rule file's header carries, as the decision has it.
    pub status: String,
    /// The scope root, `.` for the whole repository.
    pub scope_root: String,
    /// Globs relative to `scope_root`; a `!` prefix excludes.
    pub scope_paths: Vec<String>,
    pub policies: Vec<Policy>,
}

/// `source`'s rule file: its name under `.actual/rules/` and its text. `name`
/// keeps an existing file's name, which its header repeats; `None` takes the
/// name the sync gives the decision.
pub fn render_rule_file(source: &RuleSource, name: Option<&str>) -> (String, String) {
    let name = name.map_or_else(
        || rule_file_name(&source.adr_id, &source.title),
        str::to_string,
    );
    let header = render_header(&AdrHeader {
        name: name.trim_end_matches(".md").to_string(),
        description: description_for(&source.title),
        adr_id: source.adr_id.clone(),
        status: source.status.clone(),
        scope_root: source.scope_root.clone(),
        scope_paths: source.scope_paths.clone(),
    });
    let rules: Vec<(&str, &str)> = source
        .policies
        .iter()
        .map(|policy| (rule_spelling(policy.level), policy.statement.as_str()))
        .collect();
    let body = render_rule_body(
        &source.title,
        &scope_line(&source.scope_root, &source.scope_paths),
        &rule_id_prefix(&source.adr_id),
        &rules,
    );
    (name, format!("{header}\n{body}"))
}

/// What the record keeps for `source`: the SHA-256 of its rule file under
/// the sync's name, which moves with anything the file would say.
pub fn fingerprint(source: &RuleSource) -> String {
    sha256_hex(render_rule_file(source, None).1.as_bytes())
}

/// SHA-256 of `bytes`, in hex.
pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// The id a rule file gives policy `n` (from 1) of decision `adr_id`, such as
/// `R-0B6F-001`. A decision file stores no policy ids, so the signed-out
/// commands name a policy by this one.
pub fn policy_rule_id(adr_id: &str, n: usize) -> String {
    format!("{}-{n:03}", rule_id_prefix(adr_id))
}

// ── the record ─────────────────────────────────────────────────────────────

/// Where a decision the record knows came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Origin {
    /// A decision file under `docs/adr/`, read signed out.
    DocsAdr,
    /// A decision in Actual AI, read signed in.
    ActualAi,
}

/// What the record knows about one decision's rule file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    /// The decision's [`fingerprint`] when the refresh last wrote its rule
    /// file or first met it.
    pub fingerprint: String,
    pub origin: Origin,
}

/// The connected repository a signed-in refresh last read, which the next
/// one reuses while the organization and the origin remote are the same.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectedRepo {
    pub org_id: String,
    pub remote: String,
    pub repo_id: String,
}

/// What the refresh remembers about one checkout.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    #[serde(default)]
    pub format_version: u32,
    /// Each decision's entry, by its id.
    #[serde(default)]
    pub rules: BTreeMap<String, Entry>,
    /// [`list_fingerprint`] of the unnormalized list last reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unnormalized: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connected_repo: Option<ConnectedRepo>,
}

impl Record {
    /// Drop and set entries as a refresh decided.
    pub fn apply_edits(&mut self, set: &BTreeMap<String, Entry>, drop: &[String]) {
        for id in drop {
            self.rules.remove(id);
        }
        for (id, entry) in set {
            self.rules.insert(id.clone(), entry.clone());
        }
    }
}

// ── docs/adr ───────────────────────────────────────────────────────────────

/// A file under `docs/adr/` that isn't normalized, and why. A normalized
/// file has a version 2 ADR header naming its id and passes the save check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Unnormalized {
    /// Its path from the repository root.
    pub path: String,
    /// The id its header names, when it has one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub adr_id: Option<String>,
    pub reasons: Vec<String>,
}

/// A normalized decision file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocDecision {
    pub path: PathBuf,
    pub source: RuleSource,
    /// Whether its status governs, which gives it a rule file.
    pub governs: bool,
}

/// What [`scan_docs`] read under `docs/adr/`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DocScan {
    pub decisions: Vec<DocDecision>,
    pub unnormalized: Vec<Unnormalized>,
}

/// Read every decision file under `<root>/docs/adr/`, three levels down, as
/// a normalized decision or an unnormalized file with its reasons. Anything
/// under `docs/adr/drafts/` is left out, and so is an index, a README or a
/// template that carries no ADR header. `today` dates the save check, as the
/// server's save does.
pub fn scan_docs(root: &Path, today: &str) -> DocScan {
    let mut paths = Vec::new();
    files::collect_markdown(&root.join(DECISION_DIR), files::MAX_DEPTH, &mut paths);
    paths.sort();
    let mut scan = DocScan::default();
    let mut first_with: HashMap<String, String> = HashMap::new();
    for path in paths {
        let shown = relative(root, &path);
        if in_drafts(Path::new(&shown)) {
            continue;
        }
        let file = match files::read_decision_file(&path) {
            Ok(file) => file,
            Err(e) => {
                if !listed_apart(&path) {
                    scan.unnormalized.push(Unnormalized {
                        path: shown,
                        adr_id: None,
                        reasons: vec![e.to_string()],
                    });
                }
                continue;
            }
        };
        let adr_id = file.adr_id().map(str::to_string);
        if adr_id.is_none() && listed_apart(&path) {
            continue;
        }
        if let Some(first) = adr_id.as_ref().and_then(|id| first_with.get(id)) {
            scan.unnormalized.push(Unnormalized {
                path: shown,
                adr_id,
                reasons: vec![format!("It has the same id as {first}.")],
            });
            continue;
        }
        if let Some(id) = &adr_id {
            first_with.insert(id.clone(), shown.clone());
        }
        match read_normalized(&file, file.is_draft(), today) {
            Ok(source) => scan.decisions.push(DocDecision {
                governs: file.meta.as_ref().is_some_and(|meta| meta.is_active()),
                path,
                source,
            }),
            Err(reasons) => scan.unnormalized.push(Unnormalized {
                path: shown,
                adr_id,
                reasons,
            }),
        }
    }
    scan
}

/// Whether `path`, from the repository root, is under `docs/adr/drafts/`,
/// which keeps work in progress.
fn in_drafts(path: &Path) -> bool {
    path.starts_with(Path::new(DECISION_DIR).join("drafts"))
}

/// Whether a file is an index, a README or a template by its name, as
/// `adr discover` reads names. Only a file with no ADR header is judged so.
fn listed_apart(path: &Path) -> bool {
    let stem = path
        .file_stem()
        .map(|stem| stem.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    stem == "readme" || stem == "index" || stem.ends_with("template")
}

/// Whether a file under `docs/adr/` with no ADR header could be a decision
/// someone has yet to normalize: not a draft in progress, an index, a README
/// or a template.
pub fn is_decision_candidate(path: &Path) -> bool {
    !in_drafts(path) && !listed_apart(path)
}

/// The rule source a normalized file gives, or every reason it isn't
/// normalized. `as_draft` runs the save check the way a draft's save does.
pub fn read_normalized(
    file: &DecisionFile,
    as_draft: bool,
    today: &str,
) -> Result<RuleSource, Vec<String>> {
    let (Some(meta), Some(adr_id)) = (file.meta.as_ref(), file.adr_id()) else {
        return Err(vec![NO_HEADER.to_string()]);
    };
    let mut reasons = Vec::new();
    match meta.version.as_deref() {
        Some(HEADER_VERSION) => {}
        Some(other) => reasons.push(format!(
            "Its ADR header is version {other}, not {HEADER_VERSION}."
        )),
        None => reasons.push(format!(
            "Its ADR header has no `version: {HEADER_VERSION}` line."
        )),
    }
    match check_markdown(&file.body, as_draft, today).result {
        Ok(doc) if reasons.is_empty() => Ok(RuleSource {
            adr_id: adr_id.to_string(),
            title: doc.title,
            status: meta
                .status
                .clone()
                .unwrap_or_else(|| "accepted".to_string()),
            scope_root: meta
                .root
                .clone()
                .unwrap_or_else(|| REPOSITORY_ROOT.to_string()),
            scope_paths: meta.paths.clone(),
            policies: doc.policies,
        }),
        Ok(_) => Err(reasons),
        Err(problems) => {
            reasons.extend(problems);
            Err(reasons)
        }
    }
}

/// The fingerprint the record keeps of an unnormalized list, so a refresh
/// reports the list only when it changed.
pub fn list_fingerprint(list: &[Unnormalized]) -> String {
    sha256_hex(serde_json::to_string(list).unwrap_or_default().as_bytes())
}

// ── planning ───────────────────────────────────────────────────────────────

/// One change to a rule file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    /// A governing decision with no rule file yet.
    Write { path: PathBuf, text: String },
    /// A rule file whose decision changed after it was written.
    Rewrite { path: PathBuf, text: String },
    /// A rule file whose decision no longer governs.
    Remove { path: PathBuf },
}

/// What a refresh does: each change with the decision it is for, and the
/// record entries to set and drop.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Plan {
    pub changes: Vec<(String, Change)>,
    pub set: BTreeMap<String, Entry>,
    pub drop: Vec<String>,
}

/// Builds a [`Plan`] against the rule files on disk, by the id each names,
/// and the record.
pub struct Planner<'a> {
    root: &'a Path,
    on_disk: &'a BTreeMap<String, PathBuf>,
    record: &'a Record,
    pub plan: Plan,
}

impl<'a> Planner<'a> {
    pub fn new(root: &'a Path, on_disk: &'a BTreeMap<String, PathBuf>, record: &'a Record) -> Self {
        Self {
            root,
            on_disk,
            record,
            plan: Plan::default(),
        }
    }

    /// Weigh decision `adr_id` from `origin`: `source` while it governs, else
    /// `None`. A governing decision with no rule file gets one; one whose
    /// file the record knows is rewritten when its fingerprint moved, or
    /// whenever `changed` says the caller just changed the decision; one
    /// whose file the record doesn't know is recorded as the file stands. A
    /// decision that doesn't govern loses the rule file naming it.
    pub fn decision(
        &mut self,
        adr_id: &str,
        source: Option<&RuleSource>,
        origin: Origin,
        changed: bool,
    ) {
        let plan = &mut self.plan;
        let Some(source) = source else {
            if let Some(path) = self.on_disk.get(adr_id) {
                plan.changes
                    .push((adr_id.to_string(), Change::Remove { path: path.clone() }));
            }
            if self.record.rules.contains_key(adr_id) {
                plan.drop.push(adr_id.to_string());
            }
            return;
        };
        let fingerprint = fingerprint(source);
        let known = self
            .record
            .rules
            .get(adr_id)
            .filter(|entry| entry.origin == origin);
        match (self.on_disk.get(adr_id), known) {
            (None, _) => {
                let (name, text) = render_rule_file(source, None);
                let path = self.root.join(RULES_DIR).join(name);
                plan.changes
                    .push((adr_id.to_string(), Change::Write { path, text }));
            }
            (Some(path), known)
                if changed || known.is_some_and(|entry| entry.fingerprint != fingerprint) =>
            {
                let name = path.file_name().and_then(|name| name.to_str());
                let (_, text) = render_rule_file(source, name);
                plan.changes.push((
                    adr_id.to_string(),
                    Change::Rewrite {
                        path: path.clone(),
                        text,
                    },
                ));
            }
            (Some(_), Some(_)) => return,
            (Some(_), None) => {}
        }
        plan.set.insert(
            adr_id.to_string(),
            Entry {
                fingerprint,
                origin,
            },
        );
    }

    /// A decision `origin` no longer has. Its rule file goes only when the
    /// record shows the refresh wrote or met it for that decision, so a rule
    /// file from elsewhere is never removed.
    fn gone(&mut self, adr_id: &str, origin: Origin) {
        if !self
            .record
            .rules
            .get(adr_id)
            .is_some_and(|entry| entry.origin == origin)
        {
            return;
        }
        if let Some(path) = self.on_disk.get(adr_id) {
            self.plan
                .changes
                .push((adr_id.to_string(), Change::Remove { path: path.clone() }));
        }
        self.plan.drop.push(adr_id.to_string());
    }
}

/// The plan that brings `.actual/rules/` in step with the decision files
/// under `docs/adr/`. An unnormalized file keeps the rule file its decision
/// last had.
pub fn plan_from_docs(
    root: &Path,
    scan: &DocScan,
    on_disk: &BTreeMap<String, PathBuf>,
    record: &Record,
) -> Plan {
    let mut planner = Planner::new(root, on_disk, record);
    for doc in &scan.decisions {
        planner.decision(
            &doc.source.adr_id,
            doc.governs.then_some(&doc.source),
            Origin::DocsAdr,
            false,
        );
    }
    let present: HashSet<&str> = scan
        .decisions
        .iter()
        .map(|doc| doc.source.adr_id.as_str())
        .chain(scan.unnormalized.iter().filter_map(|u| u.adr_id.as_deref()))
        .collect();
    for id in record.rules.keys() {
        if !present.contains(id.as_str()) {
            planner.gone(id, Origin::DocsAdr);
        }
    }
    planner.plan
}

/// The plan that brings `.actual/rules/` in step with the repository's
/// decisions in Actual AI: each decision's id with its rule source while it
/// is active. A decision listed twice counts once.
pub fn plan_from_cloud(
    root: &Path,
    decisions: &[(String, Option<RuleSource>)],
    on_disk: &BTreeMap<String, PathBuf>,
    record: &Record,
) -> Plan {
    let mut planner = Planner::new(root, on_disk, record);
    let mut listed = HashSet::new();
    for (id, source) in decisions {
        if listed.insert(id.as_str()) {
            planner.decision(id, source.as_ref(), Origin::ActualAi, false);
        }
    }
    for id in record.rules.keys() {
        if !listed.contains(id.as_str()) {
            planner.gone(id, Origin::ActualAi);
        }
    }
    planner.plan
}

// ── applying ───────────────────────────────────────────────────────────────

/// What applying a plan did to `.actual/rules/`, each path from the
/// repository root.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Applied {
    pub written: Vec<String>,
    pub rewritten: Vec<String>,
    pub removed: Vec<String>,
    /// Each change that failed, with the reason.
    pub problems: Vec<String>,
}

impl Applied {
    /// How many rule files changed.
    pub fn changed(&self) -> usize {
        self.written.len() + self.rewritten.len() + self.removed.len()
    }
}

/// Make the plan's changes. Returns what happened, and the plan's record
/// edits less those of any decision whose change failed, so the next
/// refresh tries it again.
pub fn apply(root: &Path, plan: Plan) -> (Applied, BTreeMap<String, Entry>, Vec<String>) {
    let mut applied = Applied::default();
    let mut failed = HashSet::new();
    for (id, change) in &plan.changes {
        let (path, result, done) = match change {
            Change::Write { path, text } => {
                (path, files::write_atomic(path, text), &mut applied.written)
            }
            Change::Rewrite { path, text } => (
                path,
                files::write_atomic(path, text),
                &mut applied.rewritten,
            ),
            Change::Remove { path } => (path, remove(path), &mut applied.removed),
        };
        match result {
            Ok(()) => done.push(relative(root, path)),
            Err(e) => {
                applied
                    .problems
                    .push(format!("{}: {e}", relative(root, path)));
                failed.insert(id.clone());
            }
        }
    }
    let set = plan
        .set
        .into_iter()
        .filter(|(id, _)| !failed.contains(id))
        .collect();
    let drop = plan
        .drop
        .into_iter()
        .filter(|id| !failed.contains(id))
        .collect();
    (applied, set, drop)
}

/// Remove `path`; one already gone counts as removed.
fn remove(path: &Path) -> Result<(), crate::error::ActualError> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// `path` from `root` when it is inside it.
pub fn relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .display()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::parse_rule_document;
    use crate::rules::types::RuleLevel;
    use tempfile::tempdir;

    const TODAY: &str = "2026-11-20";
    const ID: &str = "0b6f7e2a-1111-4111-8111-111111111111";

    fn source(title: &str, statement: &str) -> RuleSource {
        RuleSource {
            adr_id: ID.to_string(),
            title: title.to_string(),
            status: "accepted".to_string(),
            scope_root: ".".to_string(),
            scope_paths: Vec::new(),
            policies: vec![Policy {
                level: RuleLevel::MustNot,
                statement: statement.to_string(),
            }],
        }
    }

    /// A decision file as `actual adr create` writes one.
    fn decision_file(id: &str, status: &str, title: &str, policy: &str) -> String {
        format!(
            "---\nversion: 2\nname: {id}-x\ndescription: {title}\nmetadata:\n  actual:\n    adr_id: {id}\n    status: {status}\n  scope:\n    root: .\n---\n\n# {title}\n\nStatus: accepted\nDate: 2026-11-19\nDeciders: \n\n## Context\n\n- Why.\n\n## Decision\n\n1. {policy}\n"
        )
    }

    fn write(root: &Path, relative: &str, text: &str) -> PathBuf {
        let path = root.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, text).unwrap();
        path
    }

    #[test]
    fn a_rule_file_parses_with_its_rules_under_the_syncs_name() {
        let src = source("Use Postgres", "Add a second database.");
        let (name, text) = render_rule_file(&src, None);
        assert_eq!(name, rule_file_name(ID, "Use Postgres"));
        let path = PathBuf::from(RULES_DIR).join(&name);
        let doc = parse_rule_document(&path, &text).unwrap();
        assert!(doc.is_active());
        assert_eq!(doc.rules.len(), 1);
        assert_eq!(doc.rules[0].level, RuleLevel::MustNot);
        assert_eq!(doc.rules[0].id, "R-0B6F-001");
        assert_eq!(policy_rule_id(ID, 1), "R-0B6F-001");
        assert!(doc.warnings.is_empty(), "{:?}", doc.warnings);

        // An existing file keeps its own name, which its header repeats.
        let (kept, text) = render_rule_file(&src, Some("security-db-1a2b.md"));
        assert_eq!(kept, "security-db-1a2b.md");
        assert!(text.contains("\nname: security-db-1a2b\n"), "{text}");
        // The fingerprint ignores the name and follows the content.
        assert_eq!(fingerprint(&src), fingerprint(&src.clone()));
        assert_ne!(
            fingerprint(&src),
            fingerprint(&source("Use Postgres", "Add a cache."))
        );
    }

    #[test]
    fn scan_reads_decisions_and_lists_each_unnormalized_file_with_its_reasons() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        write(
            root,
            "docs/adr/a.md",
            &decision_file("id-a", "accepted", "Use Postgres", "MUST: Use Postgres."),
        );
        write(
            root,
            "docs/adr/b.md",
            &decision_file("id-b", "draft", "Use Redis", "MUST: Use Redis."),
        );
        write(
            root,
            "docs/adr/plain.md",
            "# Use Kafka\n\n## Decision\n\nKafka.\n",
        );
        write(
            root,
            "docs/adr/v1.md",
            &decision_file("id-c", "accepted", "Use S3", "MUST: Use S3.")
                .replace("version: 2\n", ""),
        );
        write(
            root,
            "docs/adr/copy.md",
            &decision_file("id-a", "accepted", "Use Postgres", "MUST: Use Postgres."),
        );
        write(
            root,
            "docs/adr/empty.md",
            &decision_file("id-d", "accepted", "No rules", "Nothing."),
        );
        // Left out: an index, a template, a draft in progress, other files.
        write(root, "docs/adr/README.md", "# Decisions\n");
        write(root, "docs/adr/0000-template.md", "# Title\n");
        // An unreadable index is still an index.
        std::fs::write(root.join("docs/adr/index.md"), [0xff, 0xfe]).unwrap();
        write(root, "docs/adr/drafts/wip.md", "# WIP\n");
        write(root, "docs/adr/notes.txt", "not markdown");
        // A decision named like a template is still a decision.
        write(
            root,
            "docs/adr/id-t-use-one-pr-template.md",
            &decision_file("id-t", "accepted", "Use one PR template", "MUST: Use it."),
        );
        let scan = scan_docs(root, TODAY);
        let ids: Vec<(&str, bool)> = scan
            .decisions
            .iter()
            .map(|d| (d.source.adr_id.as_str(), d.governs))
            .collect();
        assert_eq!(ids, [("id-a", true), ("id-b", false), ("id-t", true)]);
        assert!(is_decision_candidate(Path::new(
            "docs/adr/0007-use-kafka.md"
        )));
        assert!(!is_decision_candidate(Path::new(
            "docs/adr/adr-template.md"
        )));
        assert!(!is_decision_candidate(Path::new("docs/adr/drafts/a.md")));
        assert_eq!(scan.decisions[0].source.title, "Use Postgres");
        let listed: Vec<(&str, &str)> = scan
            .unnormalized
            .iter()
            .map(|u| {
                (
                    u.path.as_str(),
                    u.reasons[0].split(" Expect").next().unwrap(),
                )
            })
            .collect();
        assert_eq!(
            listed,
            [
                ("docs/adr/copy.md", "It has the same id as docs/adr/a.md."),
                ("docs/adr/empty.md", "No policies found."),
                ("docs/adr/plain.md", NO_HEADER),
                ("docs/adr/v1.md", "Its ADR header has no `version: 2` line."),
            ],
            "{:#?}",
            scan.unnormalized
        );
        assert_eq!(scan.unnormalized[1].adr_id.as_deref(), Some("id-d"));
        assert!(scan.unnormalized[2].adr_id.is_none());
    }

    #[test]
    fn scan_names_another_header_version_and_an_unreadable_file() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        write(
            root,
            "docs/adr/v3.md",
            &decision_file("id-a", "accepted", "T", "MUST: Do it.")
                .replace("version: 2", "version: 3"),
        );
        std::fs::write(root.join("docs/adr/binary.md"), [0xff, 0xfe]).unwrap();
        let scan = scan_docs(root, TODAY);
        assert!(scan.decisions.is_empty());
        assert_eq!(
            scan.unnormalized[1].reasons[0],
            "Its ADR header is version 3, not 2."
        );
        assert!(scan.unnormalized[0].path.ends_with("binary.md"));
        assert_ne!(
            list_fingerprint(&scan.unnormalized),
            list_fingerprint(&scan.unnormalized[..1])
        );
    }

    fn on_disk(root: &Path) -> BTreeMap<String, PathBuf> {
        files::rule_files_by_id(root)
    }

    fn run(root: &Path, record: &mut Record) -> Applied {
        let scan = scan_docs(root, TODAY);
        let plan = plan_from_docs(root, &scan, &on_disk(root), record);
        let (applied, set, drop) = apply(root, plan);
        record.apply_edits(&set, &drop);
        applied
    }

    #[test]
    fn docs_refresh_writes_rewrites_and_removes_accepted_decisions_rule_files() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        let doc = write(
            root,
            "docs/adr/a.md",
            &decision_file(ID, "accepted", "Use Postgres", "MUST: Use Postgres."),
        );
        write(
            root,
            "docs/adr/b.md",
            &decision_file("id-b", "draft", "Use Redis", "MUST: Use Redis."),
        );
        let mut record = Record::default();

        let first = run(root, &mut record);
        let name = rule_file_name(ID, "Use Postgres");
        assert_eq!(first.written, [format!("{RULES_DIR}/{name}")]);
        assert!(first.problems.is_empty());
        assert_eq!(record.rules[ID].origin, Origin::DocsAdr);
        // Nothing changed, so nothing is written again.
        assert_eq!(run(root, &mut record), Applied::default());

        // An edit to the decision rewrites its rule file.
        std::fs::write(
            &doc,
            decision_file(
                ID,
                "accepted",
                "Use Postgres",
                "MUST NOT: Add a second database.",
            ),
        )
        .unwrap();
        let edited = run(root, &mut record);
        assert_eq!(edited.rewritten, [format!("{RULES_DIR}/{name}")]);
        let text = std::fs::read_to_string(root.join(RULES_DIR).join(&name)).unwrap();
        assert!(text.contains("MUST NOT: Add a second database."), "{text}");

        // A deleted decision file takes its rule file with it.
        std::fs::remove_file(&doc).unwrap();
        let gone = run(root, &mut record);
        assert_eq!(gone.removed, [format!("{RULES_DIR}/{name}")]);
        assert!(record.rules.is_empty());
        assert!(!root.join(RULES_DIR).join(&name).exists());
    }

    #[test]
    fn docs_refresh_records_a_file_it_did_not_write_and_rewrites_it_only_after_a_change() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        let doc = write(
            root,
            "docs/adr/a.md",
            &decision_file(ID, "accepted", "Use Postgres", "MUST: Use Postgres."),
        );
        // A rule file the sync wrote, with its own words and name.
        let synced = write(
            root,
            ".actual/rules/data-postgres-9f9f.md",
            &format!(
                "---\nversion: 2\nname: data-postgres-9f9f\nmetadata:\n  actual:\n    adr_id: {ID}\n    status: accepted\n---\n\n# Postgres\n\n### Rules\n\n- **R-0B6F-001** MUST: Store data in Postgres.\n"
            ),
        );
        let before = std::fs::read_to_string(&synced).unwrap();
        let mut record = Record::default();
        assert_eq!(run(root, &mut record), Applied::default());
        assert_eq!(std::fs::read_to_string(&synced).unwrap(), before);
        assert!(record.rules.contains_key(ID));

        std::fs::write(
            &doc,
            decision_file(ID, "accepted", "Use Postgres", "SHOULD: Use Postgres."),
        )
        .unwrap();
        let edited = run(root, &mut record);
        assert_eq!(
            edited.rewritten,
            [format!("{RULES_DIR}/data-postgres-9f9f.md")]
        );
        assert!(std::fs::read_to_string(&synced)
            .unwrap()
            .contains("SHOULD: Use Postgres."));
    }

    #[test]
    fn docs_refresh_leaves_rule_files_from_elsewhere_and_an_unnormalized_decisions_file() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        let hand = write(
            root,
            ".actual/rules/hand.md",
            "# Our rules\n\n### Rules\n\n- **R-1** MUST: Be kind.\n",
        );
        let cloud = write(
            root,
            ".actual/rules/cloud.md",
            "---\nversion: 2\nmetadata:\n  actual:\n    adr_id: cloud-1\n---\n# T\n",
        );
        let doc = write(
            root,
            "docs/adr/a.md",
            &decision_file(ID, "accepted", "Use Postgres", "MUST: Use Postgres."),
        );
        let mut record = Record::default();
        assert_eq!(run(root, &mut record).written.len(), 1);
        // The decision file breaks: its last rule file stays while it's listed.
        std::fs::write(
            &doc,
            decision_file(ID, "accepted", "Use Postgres", "Use it."),
        )
        .unwrap();
        assert_eq!(run(root, &mut record), Applied::default());
        assert!(root
            .join(RULES_DIR)
            .join(rule_file_name(ID, "Use Postgres"))
            .exists());
        assert!(hand.exists() && cloud.exists());
    }

    #[test]
    fn docs_refresh_removes_a_draft_decisions_rule_file_and_any_status_that_stops_governing() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        let doc = write(
            root,
            "docs/adr/a.md",
            &decision_file(ID, "accepted", "Use Postgres", "MUST: Use Postgres."),
        );
        let mut record = Record::default();
        run(root, &mut record);
        std::fs::write(
            &doc,
            decision_file(ID, "superseded", "Use Postgres", "MUST: Use Postgres."),
        )
        .unwrap();
        let retired = run(root, &mut record);
        assert_eq!(retired.removed.len(), 1);
        assert!(record.rules.is_empty());

        // A draft's rule file goes even when the record never met it, as an
        // older `adr new` left one beside each draft.
        let draft_rule = write(
            root,
            ".actual/rules/old-draft.md",
            "---\nversion: 2\nmetadata:\n  actual:\n    adr_id: id-b\n    status: draft\n---\n# T\n",
        );
        write(
            root,
            "docs/adr/b.md",
            &decision_file("id-b", "draft", "Use Redis", "MUST: Use Redis."),
        );
        assert_eq!(
            run(root, &mut record).removed,
            [".actual/rules/old-draft.md"]
        );
        assert!(!draft_rule.exists());
    }

    #[test]
    fn cloud_plan_writes_active_decisions_and_removes_the_ones_that_left() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        let active = source("Use Postgres", "Add a second database.");
        let mut record = Record::default();
        let decisions = vec![
            (ID.to_string(), Some(active.clone())),
            ("archived-1".to_string(), None),
        ];
        let archived_rule = write(
            root,
            ".actual/rules/archived.md",
            "---\nversion: 2\nmetadata:\n  actual:\n    adr_id: archived-1\n---\n# T\n",
        );
        let plan = plan_from_cloud(root, &decisions, &on_disk(root), &record);
        let (applied, set, drop) = apply(root, plan);
        record.apply_edits(&set, &drop);
        assert_eq!(applied.written.len(), 1);
        assert_eq!(applied.removed, [".actual/rules/archived.md"]);
        assert!(!archived_rule.exists());
        assert_eq!(record.rules[ID].origin, Origin::ActualAi);

        // Unchanged: nothing. Listed twice: once.
        let twice = vec![
            (ID.to_string(), Some(active.clone())),
            (ID.to_string(), Some(active.clone())),
        ];
        let plan = plan_from_cloud(root, &twice, &on_disk(root), &record);
        assert!(plan.changes.is_empty() && plan.set.is_empty());

        // Gone from the list: the file the refresh wrote goes too.
        let plan = plan_from_cloud(root, &[], &on_disk(root), &record);
        let (applied, set, drop) = apply(root, plan);
        record.apply_edits(&set, &drop);
        assert_eq!(applied.removed.len(), 1);
        assert!(record.rules.is_empty());
    }

    #[test]
    fn a_record_from_the_other_origin_is_met_again_rather_than_trusted() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        let active = source("Use Postgres", "Add a second database.");
        let (name, text) = render_rule_file(&active, None);
        write(root, &format!("{RULES_DIR}/{name}"), &text);
        let mut record = Record::default();
        record.rules.insert(
            ID.to_string(),
            Entry {
                fingerprint: "stale".to_string(),
                origin: Origin::DocsAdr,
            },
        );
        let plan = plan_from_cloud(
            root,
            &[(ID.to_string(), Some(active.clone()))],
            &on_disk(root),
            &record,
        );
        assert!(plan.changes.is_empty(), "{:?}", plan.changes);
        assert_eq!(plan.set[ID].origin, Origin::ActualAi);
        // A decision the caller just changed is rewritten whatever the record says.
        let disk = on_disk(root);
        let mut forced = Planner::new(root, &disk, &record);
        forced.decision(ID, Some(&active), Origin::DocsAdr, true);
        assert!(matches!(forced.plan.changes[0].1, Change::Rewrite { .. }));
    }

    #[test]
    fn a_failed_change_leaves_its_record_entry_for_the_next_refresh() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join(".actual")).unwrap();
        // A file where the rules folder should be.
        std::fs::write(root.join(RULES_DIR), "").unwrap();
        let plan = plan_from_cloud(
            root,
            &[(ID.to_string(), Some(source("T", "Do it.")))],
            &BTreeMap::new(),
            &Record::default(),
        );
        let (applied, set, drop) = apply(root, plan);
        assert_eq!(applied.problems.len(), 1, "{applied:?}");
        assert!(applied.written.is_empty() && set.is_empty() && drop.is_empty());
        assert_eq!(applied.changed(), 0);
    }

    #[test]
    fn a_record_round_trips_and_applies_edits() {
        let mut record = Record {
            format_version: RECORD_FORMAT,
            ..Record::default()
        };
        let entry = Entry {
            fingerprint: "f".to_string(),
            origin: Origin::ActualAi,
        };
        record.apply_edits(&BTreeMap::from([("a".to_string(), entry.clone())]), &[]);
        record.unnormalized = Some("u".to_string());
        record.connected_repo = Some(ConnectedRepo {
            org_id: "o".to_string(),
            remote: "r".to_string(),
            repo_id: "i".to_string(),
        });
        let json = serde_json::to_string(&record).unwrap();
        assert!(json.contains("\"origin\":\"actual-ai\""), "{json}");
        assert_eq!(serde_json::from_str::<Record>(&json).unwrap(), record);
        record.apply_edits(&BTreeMap::new(), &["a".to_string()]);
        assert!(record.rules.is_empty());
    }
}
