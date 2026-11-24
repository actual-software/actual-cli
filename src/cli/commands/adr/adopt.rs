//! `actual adr discover` and `actual adr normalize`: bring decision docs
//! written outside Actual — by hand, or by a coding agent — under the rule
//! files the checks read, and keep them there as the docs change.
//!
//! Both work signed out. Signed in, `discover` also compares synced
//! decisions with Actual AI, and `normalize --push` saves what it writes.

use std::cell::{Cell, OnceCell};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use indicatif::{ProgressBar, ProgressDrawTarget, ProgressStyle};
use serde::Serialize;
use uuid::Uuid;

use super::local::{cloud_decisions, CloudComparison};
use super::{interactive, json_text, runtime, today};
use crate::adr::context_file::{js_trim, ContextFile};
use crate::adr::discovery::{self, relative, Entry, State};
use crate::adr::files::{self, DECISION_DIR, RULES_DIR};
use crate::adr::header::{normalize_scope_root, REPOSITORY_ROOT};
use crate::adr::names::{decision_doc_file_name, rule_file_name};
use crate::adr::normalize::{self, Extraction, Plan, Question, Read, Settings};
use crate::adr::overlap::ExistingDecision;
use crate::adr::pair::Decision;
use crate::adr::save_check::check_document;
use crate::api::adr_types::SaveStatus;
use crate::cli::args::{AdrApiOptions, AdrCreateArgs, AdrDiscoverArgs, AdrNormalizeArgs};
use crate::cli::commands::rules_rank;
use crate::cli::commands::sync::resolve_cwd;
use crate::cli::ui::real_terminal::RealTerminal;
use crate::cli::ui::terminal::TerminalIO;
use crate::cli::ui::theme;
use crate::error::{sanitize_server_text, ActualError};
use crate::rules::scope::signals::PathGlob;
use crate::runner::structured::StructuredRunner;

// ── discover ───────────────────────────────────────────────────────────────

/// What `discover` prints.
#[derive(Debug, Clone, PartialEq, Serialize)]
struct DiscoverReport {
    entries: Vec<Entry>,
    /// How many files are in each state.
    counts: BTreeMap<&'static str, usize>,
    cloud: CloudComparison,
}

pub(super) fn exec_discover(args: &AdrDiscoverArgs) -> Result<(), ActualError> {
    let root = args.root.clone().unwrap_or_else(resolve_cwd);
    let report = runtime()?.block_on(run_discover(
        args.no_cloud,
        args.repo.as_deref(),
        &args.api,
        &root,
    ));
    if args.api.json {
        println!("{}", json_text(&report));
    } else {
        print!("{}", render_discover(&report));
    }
    Ok(())
}

async fn run_discover(
    no_cloud: bool,
    repo: Option<&str>,
    api: &AdrApiOptions,
    root: &Path,
) -> DiscoverReport {
    let mut entries = discovery::discover(root);
    mark_uncommitted(root, &mut entries);
    let any_synced = entries.iter().any(|entry| entry.state == State::Synced);
    let (cloud, decisions) = if any_synced {
        cloud_decisions(no_cloud, repo, api, root).await
    } else {
        (
            CloudComparison {
                note: Some("No synced decisions to compare with Actual AI.".to_string()),
                ..CloudComparison::default()
            },
            Vec::new(),
        )
    };
    if cloud.compared {
        compare_with_cloud(&mut entries, &decisions);
    }
    let mut counts = BTreeMap::new();
    for (state, count) in discovery::counts(&entries) {
        counts.insert(state.as_str(), count);
    }
    for entry in &mut entries {
        entry.path = PathBuf::from(relative(root, &entry.path));
        entry.rule_file = entry
            .rule_file
            .as_ref()
            .map(|path| PathBuf::from(relative(root, path)));
        entry.legacy_files = entry
            .legacy_files
            .iter()
            .map(|path| PathBuf::from(relative(root, path)))
            .collect();
    }
    DiscoverReport {
        entries,
        counts,
        cloud,
    }
}

/// The files at or under `paths` that git reports as changed and not
/// committed. Outside a git checkout, or when git fails, none.
fn uncommitted(root: &Path, paths: &[&Path]) -> HashSet<PathBuf> {
    if paths.is_empty() {
        return HashSet::new();
    }
    changed_in_git(root, paths).unwrap_or_default()
}

fn changed_in_git(root: &Path, paths: &[&Path]) -> Option<HashSet<PathBuf>> {
    let git = |args: &[&std::ffi::OsStr]| {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .output()
            .ok()
            .filter(|output| output.status.success())?;
        Some(String::from_utf8_lossy(&output.stdout).into_owned())
    };
    // git names paths from the checkout's top level, which `root` may be
    // below: `pkg/` when it is `<top>/pkg`.
    let prefix = git(&["rev-parse".as_ref(), "--show-prefix".as_ref()])?;
    let prefix = prefix.trim_end_matches('\n');
    let mut args: Vec<&std::ffi::OsStr> = ["status", "--porcelain", "-z", "--"]
        .iter()
        .map(|arg| arg.as_ref())
        .collect();
    args.extend(paths.iter().map(|path| path.as_os_str()));
    let status = git(&args)?;
    let mut changed = HashSet::new();
    let mut records = status.split('\0');
    while let Some(record) = records.next() {
        let (Some(code), Some(path)) = (record.get(..2), record.get(3..)) else {
            continue;
        };
        // A rename or copy is followed by the path it came from.
        if code.starts_with(['R', 'C']) {
            records.next();
        }
        if let Some(path) = path.strip_prefix(prefix).filter(|path| !path.is_empty()) {
            changed.insert(root.join(path));
        }
    }
    Some(changed)
}

/// A synced doc with uncommitted changes has been edited here; the next
/// sync overwrites the edit.
fn mark_uncommitted(root: &Path, entries: &mut [Entry]) {
    // Their folders, not the files, so git can pair a rename's two paths.
    let folders: Vec<&Path> = entries
        .iter()
        .filter(|entry| entry.state == State::Synced)
        .filter_map(|entry| entry.path.parent())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    let changed = uncommitted(root, &folders);
    for entry in entries.iter_mut() {
        if entry.state == State::Synced && changed.contains(&entry.path) {
            entry.set_state(
                State::SyncedEdited,
                format!(
                    "It has uncommitted changes, and the next sync overwrites them. {}",
                    push_hint(entry.adr_id.as_deref())
                ),
            );
        }
    }
}

fn push_hint(id: Option<&str>) -> String {
    format!(
        "Save the edit to Actual AI with `actual adr update {}` (or in the web app), or revert it.",
        id.map_or_else(|| "<id>".to_string(), sanitize_server_text)
    )
}

/// Text compared without case or runs of whitespace.
fn loose(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Mark each synced doc whose title or policies differ from its decision in
/// Actual AI as edited. A synced doc whose decision isn't in the list stays
/// synced, with the reason noted: the list leaves out archived decisions and
/// other repositories', so absence doesn't show who wrote the doc. A decision
/// Actual AI grouped is skipped: its files carry the ids of its parts, which
/// the list doesn't show.
fn compare_with_cloud(entries: &mut [Entry], decisions: &[ExistingDecision]) {
    for entry in entries
        .iter_mut()
        .filter(|entry| entry.state == State::Synced && entry.group_id.is_none())
    {
        let (Some(id), Some(doc)) = (entry.adr_id.clone(), entry.doc.clone()) else {
            continue;
        };
        match decisions.iter().find(|decision| decision.id == id) {
            Some(decision) => {
                let mut ours: Vec<String> =
                    doc.policies.iter().map(|p| loose(&p.statement)).collect();
                let mut theirs: Vec<String> = decision.policies.iter().map(|p| loose(p)).collect();
                ours.sort();
                theirs.sort();
                let changed: Vec<&str> = [
                    ("title", loose(&doc.title) != loose(&decision.title)),
                    ("policies", ours != theirs),
                ]
                .into_iter()
                .filter(|(_, changed)| *changed)
                .map(|(part, _)| part)
                .collect();
                if !changed.is_empty() {
                    entry.set_state(
                        State::SyncedEdited,
                        format!(
                            "Its {} differ from the decision in Actual AI, and the next sync \
                             overwrites them. {}",
                            changed.join(" and "),
                            push_hint(Some(&id))
                        ),
                    );
                }
            }
            None if entry.rule_file.is_some() => {
                entry.reason = "Not among the decisions compared in Actual AI: archived, in \
                                another repository, or written locally before its source was \
                                recorded."
                    .to_string();
            }
            None => {}
        }
    }
}

fn render_discover(report: &DiscoverReport) -> String {
    let mut out = String::new();
    if report.entries.is_empty() {
        out.push_str(&format!("No ADRs under {DECISION_DIR}/.\n"));
        return out;
    }
    out.push_str(&format!(
        "{} {}\n",
        theme::heading("ADRs under"),
        theme::heading(format!("{DECISION_DIR}/"))
    ));
    let width = report
        .entries
        .iter()
        .map(|entry| entry.state.as_str().len())
        .max()
        .unwrap_or(0);
    for entry in &report.entries {
        let title = entry
            .title
            .as_deref()
            .map(|title| format!("  {}", sanitize_server_text(title)))
            .unwrap_or_default();
        out.push_str(&format!(
            "  {:width$}  {}{title}\n      {}\n",
            entry.state.as_str(),
            entry.path.display(),
            theme::muted(&entry.reason),
        ));
        for path in &entry.legacy_files {
            out.push_str(&format!("      beside it: {}\n", path.display()));
        }
    }
    let todo = report
        .entries
        .iter()
        .filter(|entry| entry.action.normalizes())
        .count();
    if todo > 0 {
        out.push_str(&format!(
            "\n{todo} {} to normalize or re-index: `actual adr normalize --all` (add --dry-run \
             to preview).\n",
            if todo == 1 { "file" } else { "files" }
        ));
    } else {
        out.push_str("\nNothing to normalize.\n");
    }
    if let Some(note) = &report.cloud.note {
        out.push_str(&format!("{note}\n"));
    }
    out
}

// ── normalize ──────────────────────────────────────────────────────────────

/// What happened to one file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum Outcome {
    Written,
    WouldWrite,
    Reindexed,
    WouldReindex,
    NeedsInput,
    Skipped,
    Unchanged,
}

/// One file's result.
#[derive(Debug, Clone, PartialEq, Serialize)]
struct FileOutcome {
    path: String,
    state: State,
    outcome: Outcome,
    #[serde(skip_serializing_if = "Option::is_none")]
    adr_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    decision_file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    rule_file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    questions: Vec<Question>,
    /// Safe answers `--yes` took.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    defaulted: Vec<String>,
    /// Files left for a person to tidy, such as adr-bot's.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    follow_ups: Vec<String>,
    /// For `--dry-run`, the doc's change as a unified diff.
    #[serde(skip_serializing_if = "Option::is_none")]
    diff: Option<String>,
    /// For `--push`, what `actual adr create --file` returned.
    #[serde(skip_serializing_if = "Option::is_none")]
    pushed: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    push_problem: Option<String>,
}

impl FileOutcome {
    fn new(path: String, state: State, outcome: Outcome) -> Self {
        FileOutcome {
            path,
            state,
            outcome,
            adr_id: None,
            status: None,
            decision_file: None,
            rule_file: None,
            reason: None,
            questions: Vec::new(),
            defaulted: Vec::new(),
            follow_ups: Vec::new(),
            diff: None,
            pushed: None,
            push_problem: None,
        }
    }

    fn skipped(path: String, state: State, reason: impl Into<String>) -> Self {
        FileOutcome {
            reason: Some(reason.into()),
            ..FileOutcome::new(path, state, Outcome::Skipped)
        }
    }
}

/// What `normalize` prints.
#[derive(Debug, Clone, PartialEq, Serialize)]
struct NormalizeReport {
    dry_run: bool,
    files: Vec<FileOutcome>,
    /// The runner used for decisions written as prose, or why none was.
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<String>,
}

pub(super) fn exec_normalize(args: &AdrNormalizeArgs) -> Result<(), ActualError> {
    let root = args.root.clone().unwrap_or_else(resolve_cwd);
    let terminal = RealTerminal::default();
    let ask: Option<&dyn TerminalIO> =
        (interactive() && !args.yes && !args.api.json).then_some(&terminal);
    // Probing a runner starts a runtime of its own, so it runs on a thread
    // outside the one driving this command.
    let resolve = || {
        std::thread::scope(|scope| {
            scope
                .spawn(|| {
                    let cfg = crate::config::paths::load().unwrap_or_default();
                    rules_rank::resolve(
                        args.runner.as_ref(),
                        args.model.as_deref(),
                        &cfg,
                        MODEL_TIMEOUT_SECS,
                    )
                    .map(model_from)
                })
                .join()
                .unwrap_or(Err(PROBE_FAILED.to_string()))
        })
    };
    let progress = Progress::new(args.api.json);
    let today = today();
    let normalizing = run_normalize(args, &root, &today, ask, &progress, resolve);
    let report = runtime()?.block_on(normalizing)?;
    if args.api.json {
        println!("{}", json_text(&report));
    } else {
        print!("{}", render_normalize(&report));
    }
    unsaved_error(&report).map_or(Ok(()), Err)
}

/// The error for a run whose `--push` left files written here but not saved
/// to Actual AI, so a caller that reads only the exit code sees it.
fn unsaved_error(report: &NormalizeReport) -> Option<ActualError> {
    let unsaved = report
        .files
        .iter()
        .filter(|file| file.push_problem.is_some())
        .count();
    (unsaved > 0).then(|| {
        ActualError::AdrNotSaved(format!(
            "{unsaved} file{} written here but not saved to Actual AI; see the report.",
            if unsaved == 1 { " was" } else { "s were" }
        ))
    })
}

/// What `normalize` shows on standard error while a model reads decisions
/// written as prose, which can take minutes for many files: a spinner while
/// it finds a runner, then a count and the file being read. Nothing is drawn
/// under `--json`, or when standard error isn't a terminal, so agents and
/// pipes see only the report.
struct Progress(ProgressBar);

impl Progress {
    fn new(json: bool) -> Self {
        let bar = ProgressBar::with_draw_target(None, ProgressDrawTarget::stderr());
        if json {
            bar.set_draw_target(ProgressDrawTarget::hidden());
        }
        Progress(bar)
    }

    /// Finding a runner for `count` docs.
    fn finding_model(&self, count: usize) {
        self.0.set_style(
            ProgressStyle::with_template("{spinner} {msg} ({elapsed})").expect("valid template"),
        );
        self.0.set_message(format!(
            "Finding a model to read {count} decision{} written as prose",
            if count == 1 { "" } else { "s" }
        ));
        self.0.enable_steady_tick(Duration::from_millis(120));
    }

    /// Reading `count` docs with the runner `label`.
    fn reading(&self, count: usize, label: &str) {
        let label = label.replace('{', "{{").replace('}', "}}");
        self.0.set_message("");
        self.0.set_style(
            ProgressStyle::with_template(&format!(
                "{{spinner}} Reading with {label} [{{pos}}/{{len}}] {{msg}} ({{elapsed}})"
            ))
            .expect("valid template"),
        );
        self.0.set_length(count as u64);
    }

    /// The doc being read now.
    fn file(&self, rel: &str) {
        self.0.set_message(rel.to_string());
    }

    fn done_one(&self) {
        self.0.inc(1);
    }

    /// Clear the line, before any question or the report.
    fn finish(&self) {
        self.0.finish_and_clear();
    }
}

/// How long one extraction may run.
const MODEL_TIMEOUT_SECS: u64 = 180;

const PROBE_FAILED: &str = "probing the runners failed";

fn model_from(resolved: rules_rank::ResolvedRunner) -> Model<rules_rank::SelectionRunner> {
    Model {
        label: resolved.label(),
        model: resolved.model.clone(),
        max_budget_usd: resolved.max_budget_usd,
        runner: resolved.runner,
    }
}

/// A runner for extraction, with what to call it.
struct Model<R> {
    runner: R,
    model: Option<String>,
    /// The configured cap on each call, which only the Claude CLI enforces.
    max_budget_usd: Option<f64>,
    label: String,
}

/// Per-file JSON from `--extracted` or `--answers`, keyed by each doc's path
/// from the repository root.
fn read_keyed(path: Option<&Path>) -> Result<HashMap<String, Extraction>, ActualError> {
    let Some(path) = path else {
        return Ok(HashMap::new());
    };
    let text = std::fs::read_to_string(path)
        .map_err(|e| ActualError::ConfigError(format!("{}: {e}", path.display())))?;
    serde_json::from_str(&text).map_err(|e| {
        ActualError::ConfigError(format!(
            "{} isn't a JSON object of extractions keyed by path: {e}",
            path.display()
        ))
    })
}

/// The entries `normalize` acts on: the named files, or with `--all` every
/// one that needs it.
fn targets(
    args: &AdrNormalizeArgs,
    root: &Path,
    entries: Vec<Entry>,
) -> Result<Vec<Entry>, ActualError> {
    if args.all {
        return Ok(entries
            .into_iter()
            .filter(|entry| {
                entry.action.normalizes() || (args.legacy && entry.state == State::Legacy)
            })
            .collect());
    }
    let canonical = |path: &Path| path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let mut by_path: HashMap<PathBuf, Entry> = entries
        .into_iter()
        .map(|entry| (canonical(&entry.path), entry))
        .collect();
    args.files
        .iter()
        .map(|file| {
            let path = if file.is_absolute() {
                file.clone()
            } else {
                root.join(file)
            };
            by_path.remove(&canonical(&path)).ok_or_else(|| {
                ActualError::ConfigError(format!(
                    "{} isn't a markdown file under {}/{DECISION_DIR}/",
                    file.display(),
                    root.display()
                ))
            })
        })
        .collect()
}

/// The repository's files, listed once, for checking scope globs.
struct RepoFiles<'a> {
    root: &'a Path,
    /// The most files listed; past it a glob is taken to match.
    limit: usize,
    files: OnceCell<Vec<String>>,
}

/// The most files listed for scope checks.
const MAX_REPO_FILES: usize = 50_000;

impl<'a> RepoFiles<'a> {
    fn new(root: &'a Path) -> Self {
        RepoFiles {
            root,
            limit: MAX_REPO_FILES,
            files: OnceCell::new(),
        }
    }

    /// Every file git doesn't ignore, hidden ones included (a scope such as
    /// `.github/workflows/**` is real), less git's own.
    fn list(&self) -> &[String] {
        self.files.get_or_init(|| {
            ignore::WalkBuilder::new(self.root)
                .hidden(false)
                .filter_entry(|entry| entry.file_name() != ".git")
                .build()
                .flatten()
                .filter(|entry| entry.file_type().is_some_and(|kind| kind.is_file()))
                .map(|entry| relative(self.root, entry.path()))
                .take(self.limit)
                .collect()
        })
    }

    /// Whether `glob` matches a file, or names a directory that holds one,
    /// by the scope index's rules (brace lists expand). A listing cut short
    /// can't say no, so it says yes.
    fn matches(&self, glob: &str) -> bool {
        let glob = glob.trim_start_matches("./");
        let pattern = PathGlob {
            pattern: glob.to_string(),
        };
        let dir = format!("{}/", glob.trim_end_matches("/**").trim_end_matches('/'));
        let files = self.list();
        files.len() >= self.limit
            || files
                .iter()
                .any(|file| pattern.matches_path(file) == Some(true) || file.starts_with(&dir))
    }

    /// The top-level directories, for a model to name a scope with.
    fn top_dirs(&self) -> Vec<String> {
        let mut dirs: Vec<String> = self
            .list()
            .iter()
            .filter_map(|file| file.split_once('/').map(|(dir, _)| dir.to_string()))
            .filter(|dir| !dir.starts_with('.'))
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        dirs.sort();
        dirs.truncate(40);
        dirs
    }
}

/// A file read and mapped, before any model or question.
struct Prepared {
    entry: Entry,
    rel: String,
    text: String,
    read: Read,
    merged: Extraction,
    /// A model wrote its policies.
    by_model: bool,
}

async fn run_normalize<R: StructuredRunner>(
    args: &AdrNormalizeArgs,
    root: &Path,
    today: &str,
    ask: Option<&dyn TerminalIO>,
    progress: &Progress,
    resolve: impl FnOnce() -> Result<Model<R>, String>,
) -> Result<NormalizeReport, ActualError> {
    let extracted = read_keyed(args.extracted.as_deref())?;
    let answers = read_keyed(args.answers.as_deref())?;
    let keyed = |map: &HashMap<String, Extraction>, rel: &str, abs: &Path| {
        map.get(rel)
            .or_else(|| map.get(&abs.display().to_string()))
            .cloned()
    };
    let repo_files = RepoFiles::new(root);

    let mut files: Vec<FileOutcome> = Vec::new();
    let mut converting: Vec<Prepared> = Vec::new();
    for entry in targets(args, root, discovery::discover(root))? {
        let rel = relative(root, &entry.path);
        match (entry.state, entry.doc.clone()) {
            (State::Stale | State::MissingRule, Some(doc)) => {
                files.push(reindex(args, root, &entry, doc, today)?);
                continue;
            }
            (State::Managed, _) => {
                let mut outcome = FileOutcome::new(rel, entry.state, Outcome::Unchanged);
                outcome.adr_id = entry.adr_id.clone();
                files.push(outcome);
                continue;
            }
            (State::Legacy, _) if !args.legacy => {
                files.push(FileOutcome::skipped(
                    rel,
                    entry.state,
                    "Written by `actual adr-bot`; pass --legacy to convert it in place.",
                ));
                continue;
            }
            (State::Synced | State::SyncedEdited, _) => {
                files.push(FileOutcome::skipped(
                    rel,
                    entry.state,
                    format!(
                        "Actual AI's sync owns this decision. {}",
                        push_hint(entry.adr_id.as_deref())
                    ),
                ));
                continue;
            }
            (State::Ignored | State::Unreadable | State::OrphanRule, _) => {
                files.push(FileOutcome::skipped(rel, entry.state, entry.reason.clone()));
                continue;
            }
            _ => {}
        }
        let text = std::fs::read_to_string(&entry.path)?;
        let mut read = normalize::read(&text);
        keep_declared_scope(&entry, &mut read.found);
        let mut merged = read.found.clone();
        if let Some(extraction) = keyed(&extracted, &rel, &entry.path) {
            merged = extraction.overlay(&read.found);
        }
        converting.push(Prepared {
            entry,
            rel,
            text,
            read,
            merged,
            by_model: false,
        });
    }

    // A model, only for docs no layout or extraction gave policies for.
    let wants_model =
        |p: &Prepared| !args.no_model && p.merged.needs_model() && !extracted.contains_key(&p.rel);
    let mut model_note = None;
    let prose = converting.iter().filter(|p| wants_model(p)).count();
    if prose > 0 {
        progress.finding_model(prose);
        match resolve() {
            Ok(model) => {
                model_note = Some(model.label.clone());
                let dirs = repo_files.top_dirs();
                progress.reading(prose, &model.label);
                for prepared in converting.iter_mut().filter(|p| wants_model(p)) {
                    progress.file(&prepared.rel);
                    let prompt = normalize::model_prompt(&prepared.rel, &prepared.text, &dirs);
                    let answer = model
                        .runner
                        .run_structured_json(
                            &prompt,
                            normalize::MODEL_SCHEMA,
                            model.model.as_deref(),
                            model.max_budget_usd,
                            None,
                        )
                        .await
                        .map_err(|e| e.to_string())
                        .and_then(normalize::from_model);
                    match answer {
                        Ok(mut extraction) => {
                            let found = &prepared.read.found;
                            if found.scope.is_some() || found.is_uncertain("scope") {
                                extraction.scope = None;
                                extraction.uncertain.retain(|field| field != "scope");
                            }
                            prepared.merged = extraction.overlay(&prepared.read.found);
                            prepared.by_model = true;
                        }
                        Err(reason) => {
                            model_note =
                                Some(format!("{}; {}: {reason}", model.label, prepared.rel))
                        }
                    }
                    progress.done_one();
                }
            }
            Err(reason) => model_note = Some(format!("No model ran: {reason}")),
        }
        progress.finish();
    }

    let hinted = Cell::new(false);
    for mut prepared in converting {
        if let Some(answer) = keyed(&answers, &prepared.rel, &prepared.entry.path) {
            prepared.merged = prepared.merged.overlay(&answer);
        }
        let asking = ask.map(|term| Asking {
            term,
            hinted: &hinted,
        });
        files.push(convert(args, root, today, asking, &repo_files, prepared).await?);
    }
    Ok(NormalizeReport {
        dry_run: args.dry_run,
        files,
        model: model_note,
    })
}

/// A decision written again keeps the scope it declares, and is asked about
/// when that can't be read, rather than governing everything.
fn keep_declared_scope(entry: &Entry, found: &mut Extraction) {
    if !matches!(entry.state, State::Broken | State::MissingRule) {
        return;
    }
    match declared_scope(entry) {
        Some(scope) => found.scope = Some(scope),
        None => found.uncertain.push("scope".to_string()),
    }
}

/// The scope a decision already declares, as globs from the repository
/// root: its doc's header's, or failing that its rule file's. `None` when
/// neither has a header to read.
fn declared_scope(entry: &Entry) -> Option<Vec<String>> {
    let meta = |path: &Path| files::read_decision_file(path).ok()?.meta;
    let meta = meta(&entry.path).or_else(|| meta(entry.rule_file.as_deref()?))?;
    Some(repo_globs(
        meta.root.as_deref().unwrap_or(REPOSITORY_ROOT),
        &meta.paths,
    ))
}

/// A header's `paths`, relative to its `root`, as globs from the repository
/// root. A root with no paths, or only exclusions, covers all of the root.
fn repo_globs(root: &str, paths: &[String]) -> Vec<String> {
    let root = normalize_scope_root(root);
    if root == REPOSITORY_ROOT {
        return paths.to_vec();
    }
    let to_repo = |glob: &str| format!("{root}/{}", glob.trim_start_matches("./"));
    let mut globs: Vec<String> = paths
        .iter()
        .map(|path| match path.strip_prefix('!') {
            Some(excluded) => format!("!{}", to_repo(excluded)),
            None => to_repo(path),
        })
        .collect();
    if paths.iter().all(|path| path.starts_with('!')) {
        globs.insert(0, format!("{root}/**"));
    }
    globs
}

/// Write a doc's rule file again from `doc`, the decision it reads as now.
fn reindex(
    args: &AdrNormalizeArgs,
    root: &Path,
    entry: &Entry,
    doc: ContextFile,
    today: &str,
) -> Result<FileOutcome, ActualError> {
    let rel = relative(root, &entry.path);
    let file = files::read_decision_file(&entry.path)?;
    let meta = file.meta.clone().unwrap_or_default();
    let status = meta
        .status
        .clone()
        .unwrap_or_else(|| "accepted".to_string());
    // A doc that parsed passes this unless it is over the size cap.
    let doc = match check_document(&doc, !status.eq_ignore_ascii_case("accepted"), today) {
        Ok(doc) => doc,
        Err(reasons) => return Ok(FileOutcome::skipped(rel, entry.state, reasons.join(" "))),
    };
    let adr_id = entry.adr_id.clone().unwrap_or_default();
    let decision = Decision {
        doc: &doc,
        adr_id: &adr_id,
        status: &status,
        root: meta.root.as_deref().unwrap_or(REPOSITORY_ROOT),
        scope: &meta.paths,
    };
    let rule_path = entry.rule_file.clone().unwrap_or_else(|| {
        root.join(RULES_DIR)
            .join(rule_file_name(&adr_id, &doc.title))
    });
    let rule_name = rule_path
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_default();
    let text = decision.rule_text(&rule_name, &rel);
    let outcome = if args.dry_run {
        Outcome::WouldReindex
    } else {
        write_file(&rule_path, &text)?;
        Outcome::Reindexed
    };
    Ok(FileOutcome {
        adr_id: Some(adr_id),
        status: Some(status),
        decision_file: Some(rel.clone()),
        rule_file: Some(relative(root, &rule_path)),
        ..FileOutcome::new(rel, entry.state, outcome)
    })
}

fn write_file(path: &Path, text: &str) -> Result<(), ActualError> {
    path.parent().map(std::fs::create_dir_all).transpose()?;
    std::fs::write(path, text)?;
    Ok(())
}

/// A person to ask at the terminal, and whether they've been told about
/// `--yes` yet this run.
#[derive(Clone, Copy)]
struct Asking<'a> {
    term: &'a dyn TerminalIO,
    hinted: &'a Cell<bool>,
}

/// Said once, before the first question of a run.
const YES_HINT: &str = "Pass --yes to take the safe answers without being asked: a decision \
                        you'd be asked about is written as a draft, and one with no policy is \
                        left alone.";

/// Settle and write one doc: plan it, asking a person when one is there,
/// then write its pair.
async fn convert(
    args: &AdrNormalizeArgs,
    root: &Path,
    today: &str,
    ask: Option<Asking<'_>>,
    repo_files: &RepoFiles<'_>,
    prepared: Prepared,
) -> Result<FileOutcome, ActualError> {
    let Prepared {
        entry,
        rel,
        text,
        read,
        mut merged,
        by_model,
    } = prepared;
    let stem = entry
        .path
        .file_stem()
        .map(|stem| stem.to_string_lossy().to_string())
        .unwrap_or_default();
    let fallback_title = normalize::title_from_name(&stem);
    let matches = |glob: &str| repo_files.matches(glob);
    let settings = Settings {
        duplicate: entry.state == State::PossibleDuplicate,
        allow_duplicate: args.allow_duplicate,
        yes: args.yes,
        today,
        fallback_title: &fallback_title,
        matches: &matches,
    };

    // A person answers a round at a time; the answers can raise new
    // questions (a scope that matches nothing), so a few rounds are allowed.
    let mut rounds = 0;
    let ready =
        loop {
            match normalize::plan(&read, &merged, &settings) {
                Plan::Ready(ready) => break *ready,
                Plan::Skip(reason) => return Ok(FileOutcome::skipped(rel, entry.state, reason)),
                Plan::NeedsInput(questions) => match ask {
                    Some(Asking { term, hinted }) if rounds < 3 => {
                        rounds += 1;
                        if !hinted.replace(true) {
                            term.write_line(&format!("\n{}", theme::hint(YES_HINT)));
                        }
                        term.write_line(&format!("\n{}", theme::heading(&rel)));
                        let answers = ask_questions(term, &questions)?;
                        merged = merged.overlay(&answers);
                    }
                    _ => return Ok(FileOutcome {
                        questions,
                        reason: Some(
                            "Answer the questions with --answers, or pass --yes to take the safe \
                             answers."
                                .to_string(),
                        ),
                        ..FileOutcome::new(rel, entry.state, Outcome::NeedsInput)
                    }),
                },
            }
        };

    let adr_id = entry
        .adr_id
        .clone()
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    let decision = Decision {
        doc: &ready.doc,
        adr_id: &adr_id,
        status: &ready.status,
        root: REPOSITORY_ROOT,
        scope: &ready.scope,
    };
    let (doc_path, doc_name) = if args.rename {
        let name = decision_doc_file_name(&adr_id, &ready.doc.title);
        (root.join(DECISION_DIR).join(&name), name)
    } else {
        let name = entry
            .path
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_default();
        (entry.path.clone(), name)
    };
    let rule_path = entry
        .rule_file
        .clone()
        .or_else(|| files::find_rule_file(root, &adr_id))
        .unwrap_or_else(|| {
            root.join(RULES_DIR)
                .join(rule_file_name(&adr_id, &ready.doc.title))
        });
    let rule_name = rule_path
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_default();
    let doc_rel = relative(root, &doc_path);
    let doc_text = decision.doc_text(&doc_name, &read.carried);
    let rule_text = decision.rule_text(&rule_name, &doc_rel);

    let mut outcome = FileOutcome {
        adr_id: Some(adr_id.clone()),
        status: Some(ready.status.clone()),
        decision_file: Some(doc_rel),
        rule_file: Some(relative(root, &rule_path)),
        defaulted: ready.defaulted.clone(),
        follow_ups: entry
            .legacy_files
            .iter()
            .map(|path| {
                format!(
                    "{} was written by `actual adr-bot` and is left as it is.",
                    relative(root, path)
                )
            })
            .chain(by_model.then(|| {
                "A model wrote its policies from the decision's prose; review them in the doc."
                    .to_string()
            }))
            .collect(),
        ..FileOutcome::new(rel, entry.state, Outcome::Written)
    };
    if args.dry_run {
        outcome.outcome = Outcome::WouldWrite;
        outcome.diff = Some(
            similar::TextDiff::from_lines(&text, &doc_text)
                .unified_diff()
                .header(
                    &outcome.path,
                    outcome.decision_file.as_deref().unwrap_or_default(),
                )
                .to_string(),
        );
        return Ok(outcome);
    }
    if doc_path != entry.path && doc_path.exists() {
        return Ok(FileOutcome::skipped(
            outcome.path,
            entry.state,
            format!(
                "{} already exists, so it wasn't renamed.",
                relative(root, &doc_path)
            ),
        ));
    }
    write_file(&doc_path, &doc_text)?;
    write_file(&rule_path, &rule_text)?;
    if doc_path != entry.path {
        std::fs::remove_file(&entry.path)?;
    }
    if args.push {
        push(args, &doc_path, &ready.status, &mut outcome).await;
    }
    Ok(outcome)
}

/// Save a written doc to Actual AI as `actual adr create --file` does.
async fn push(args: &AdrNormalizeArgs, doc_path: &Path, status: &str, outcome: &mut FileOutcome) {
    let status = match status {
        "accepted" => SaveStatus::Accepted,
        "draft" => SaveStatus::Draft,
        other => {
            outcome.push_problem = Some(format!(
                "A {other} decision isn't saved to Actual AI; only accepted and draft ones are."
            ));
            return;
        }
    };
    let create = AdrCreateArgs {
        file: Some(doc_path.to_path_buf()),
        title: None,
        title_flag: None,
        content: Default::default(),
        deciders: None,
        scope: Vec::new(),
        status: Some(status),
        root: None,
        repo: args.repo.clone(),
        allow_duplicate: args.allow_duplicate,
        all: false,
        api: AdrApiOptions {
            api_url: args.api.api_url.clone(),
            json: true,
        },
    };
    let dir = doc_path.parent().unwrap_or(doc_path);
    match super::cloud::run_create(&create, dir, false).await {
        Ok(json) => outcome.pushed = serde_json::from_str(&json).ok(),
        Err(e) => outcome.push_problem = Some(e.to_string()),
    }
}

/// Ask a person each question at the terminal, as an [`Extraction`] of
/// answers.
fn ask_questions(term: &dyn TerminalIO, questions: &[Question]) -> Result<Extraction, ActualError> {
    let mut answers = Extraction::default();
    for question in questions {
        match question.field {
            "keep_duplicate" => answers.keep_duplicate = Some(term.confirm(&question.question)?),
            "is_adr" => answers.is_adr = Some(term.confirm(&question.question)?),
            "status" => {
                let pick = term.select_one(&question.question, &question.options, Some(1))?;
                answers.status = question.options.get(pick).cloned();
            }
            "policies" => answers.policies = Some(ask_policies(term, question)?),
            _ => answers.scope = Some(ask_scope(term, question)?),
        }
    }
    Ok(answers)
}

fn ask_policies(term: &dyn TerminalIO, question: &Question) -> Result<Vec<String>, ActualError> {
    term.write_line(&question.question);
    if !question.suggested.is_empty() {
        for policy in &question.suggested {
            term.write_line(&format!("  {policy}"));
        }
        let choices = ["Use these".to_string(), "Enter my own".to_string()];
        if term.select_one("Policies", &choices, Some(0))? == 0 {
            return Ok(question.suggested.clone());
        }
    }
    let mut policies = Vec::new();
    loop {
        let line = term.read_line("Policy as `LEVEL: statement` (blank to finish)")?;
        let line = js_trim(&line);
        if line.is_empty() {
            return Ok(policies);
        }
        policies.push(line.to_string());
    }
}

fn ask_scope(term: &dyn TerminalIO, question: &Question) -> Result<Vec<String>, ActualError> {
    let mut choices = vec!["The whole repository".to_string()];
    if !question.suggested.is_empty() {
        choices.push(format!("Only {}", question.suggested.join(", ")));
    }
    choices.push("Enter paths".to_string());
    let pick = term.select_one(&question.question, &choices, Some(0))?;
    Ok(match pick {
        0 => Vec::new(),
        1 if !question.suggested.is_empty() => question.suggested.clone(),
        _ => term
            .read_line("Globs from the repository root, separated by commas")?
            .split(',')
            .map(|glob| js_trim(glob).to_string())
            .filter(|glob| !glob.is_empty())
            .collect(),
    })
}

fn render_normalize(report: &NormalizeReport) -> String {
    let mut out = String::new();
    if report.files.is_empty() {
        out.push_str("Nothing to normalize.\n");
    }
    for file in &report.files {
        let mark = match file.outcome {
            Outcome::Written | Outcome::Reindexed | Outcome::Unchanged => {
                theme::success(&theme::SUCCESS).to_string()
            }
            Outcome::WouldWrite | Outcome::WouldReindex => theme::hint("~").to_string(),
            Outcome::NeedsInput => theme::warning("?").to_string(),
            Outcome::Skipped => theme::muted("-").to_string(),
        };
        let what = match file.outcome {
            Outcome::Written => "normalized",
            Outcome::WouldWrite => "would normalize",
            Outcome::Reindexed => "re-indexed",
            Outcome::WouldReindex => "would re-index",
            Outcome::NeedsInput => "needs answers",
            Outcome::Skipped => "skipped",
            Outcome::Unchanged => "up to date",
        };
        out.push_str(&format!("  {mark} {}  {what}", file.path));
        if let Some(status) = &file.status {
            out.push_str(&format!(" ({status})"));
        }
        out.push('\n');
        if let (Some(doc), Some(rule)) = (&file.decision_file, &file.rule_file) {
            if matches!(file.outcome, Outcome::Written | Outcome::WouldWrite) && doc != &file.path {
                out.push_str(&format!("      -> {doc}\n"));
            }
            out.push_str(&format!("      rule file: {rule}\n"));
        }
        if let Some(reason) = &file.reason {
            out.push_str(&format!("      {reason}\n"));
        }
        for question in &file.questions {
            out.push_str(&format!(
                "      ? [{}] {}\n",
                question.field, question.question
            ));
            if !question.suggested.is_empty() {
                out.push_str(&format!(
                    "        suggested: {}\n",
                    question.suggested.join(" | ")
                ));
            }
        }
        for note in file.defaulted.iter().chain(&file.follow_ups) {
            out.push_str(&format!("      {} {note}\n", theme::warning("note:")));
        }
        if let Some(problem) = &file.push_problem {
            out.push_str(&format!("      {} {problem}\n", theme::warning("push:")));
        } else if file.pushed.is_some() {
            out.push_str("      saved to Actual AI\n");
        }
        if let Some(diff) = &file.diff {
            for line in diff.lines() {
                out.push_str(&format!("      {line}\n"));
            }
        }
    }
    if let Some(model) = &report.model {
        out.push_str(&format!("\nModel: {model}\n"));
    }
    if report
        .files
        .iter()
        .any(|file| file.outcome == Outcome::NeedsInput)
    {
        out.push_str(
            "\nSome files need answers: run again at a terminal, pass --answers <file.json> \
             (keyed by path, fields as in --json), or --yes to write them as drafts.\n",
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::super::tests::{git_repo, sign_in, CONNECTED, REPO};
    use super::*;
    use crate::adr::context_file::{self, Policy};
    use crate::adr::header::{render_header, AdrHeader};
    use crate::cli::ui::test_utils::MockTerminal;
    use crate::rules::types::RuleLevel;
    use crate::testutil::{EnvGuard, ENV_MUTEX};
    use mockito::Matcher;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tempfile::tempdir;

    const TODAY: &str = "2026-11-19";
    const ID: &str = "0b6f2a1e-1111-4222-8333-444455556666";
    const OTHER: &str = "c357aaaa-bbbb-4ccc-8ddd-eeeeffff0000";
    const THIRD: &str = "d468bbbb-cccc-4ddd-8eee-ffff00001111";

    const NYGARD: &str =
        "# 7. Use Postgres\n\n## Status\n\nAccepted\n\n## Context\n\nOne store.\n\n\
                          ## Decision\n\n- Services MUST store relational data in Postgres.\n";
    const PROSE: &str =
        "# Cache sessions\n\n## Status\n\nAccepted\n\n## Decision\n\nWe cache sessions in Redis.\n";

    const BUDGET: f64 = 0.25;

    struct Fake {
        answer: Result<serde_json::Value, String>,
        calls: AtomicUsize,
    }

    impl StructuredRunner for Fake {
        async fn run_structured_json(
            &self,
            _prompt: &str,
            _schema: &str,
            _model: Option<&str>,
            budget: Option<f64>,
            _effort: Option<&str>,
        ) -> Result<serde_json::Value, ActualError> {
            assert_eq!(budget, Some(BUDGET), "the configured cap reaches each call");
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.answer.clone().map_err(ActualError::InternalError)
        }
    }

    fn block<T>(future: impl std::future::Future<Output = T>) -> T {
        runtime().unwrap().block_on(future)
    }

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn adr(root: &Path, name: &str, text: &str) -> PathBuf {
        let path = root.join(DECISION_DIR).join(name);
        write(&path, text);
        path
    }

    fn args(files: &[&str]) -> AdrNormalizeArgs {
        AdrNormalizeArgs {
            files: files.iter().map(PathBuf::from).collect(),
            all: files.is_empty(),
            dry_run: false,
            yes: false,
            extracted: None,
            answers: None,
            rename: false,
            legacy: false,
            allow_duplicate: false,
            no_model: false,
            runner: None,
            model: None,
            push: false,
            repo: None,
            root: None,
            api: AdrApiOptions::default(),
        }
    }

    fn model_answer() -> serde_json::Value {
        serde_json::json!({
            "is_adr": true, "title": "Cache sessions", "status": "accepted",
            "context": ["Sessions are slow."], "policies": ["MUST: Sessions are cached in Redis."],
            "scope": ["src/**"], "uncertain": []
        })
    }

    /// Run `normalize` with a fake model answering `answer`, or with none.
    fn run(
        args: &AdrNormalizeArgs,
        root: &Path,
        ask: Option<&dyn TerminalIO>,
        answer: Option<Result<serde_json::Value, String>>,
    ) -> Result<NormalizeReport, ActualError> {
        block(run_normalize(
            args,
            root,
            TODAY,
            ask,
            &Progress::new(true),
            move || match answer {
                Some(answer) => Ok(Model {
                    runner: Fake {
                        answer,
                        calls: AtomicUsize::new(0),
                    },
                    model: None,
                    max_budget_usd: Some(BUDGET),
                    label: "fake".to_string(),
                }),
                None => Err("no runner here".to_string()),
            },
        ))
    }

    fn file<'a>(report: &'a NormalizeReport, name: &str) -> &'a FileOutcome {
        report
            .files
            .iter()
            .find(|f| f.path.ends_with(name))
            .unwrap_or_else(|| panic!("no result for {name}: {report:#?}"))
    }

    fn doc(title: &str, statement: &str) -> ContextFile {
        ContextFile {
            title: title.to_string(),
            status: "accepted".to_string(),
            date: TODAY.to_string(),
            context: vec!["Why.".to_string()],
            policies: vec![Policy {
                level: RuleLevel::Must,
                statement: statement.to_string(),
            }],
            ..ContextFile::default()
        }
    }

    fn local_pair(root: &Path, name: &str, id: &str, doc: &ContextFile) -> PathBuf {
        let decision = Decision {
            doc,
            adr_id: id,
            status: "accepted",
            root: REPOSITORY_ROOT,
            scope: &[],
        };
        let path = adr(root, name, &decision.doc_text(name, &[]));
        write(
            &root.join(RULES_DIR).join(format!("rule-{name}")),
            &decision.rule_text(&format!("rule-{name}"), &format!("docs/adr/{name}")),
        );
        path
    }

    fn header(id: &str, status: &str) -> String {
        render_header(&AdrHeader {
            name: "x".to_string(),
            description: "x".to_string(),
            adr_id: id.to_string(),
            status: status.to_string(),
            scope_root: ".".to_string(),
            scope_paths: Vec::new(),
            source: None,
        })
    }

    fn synced_pair(root: &Path, id: &str, title: &str) -> PathBuf {
        let path = adr(
            root,
            &format!("{id}-synced.md"),
            &format!(
                "{}\n{}",
                header(id, "accepted"),
                context_file::render(&doc(title, "Store data in Postgres."))
            ),
        );
        write(
            &root.join(RULES_DIR).join(format!("synced-{id}.md")),
            &format!("{}\n# R\n\n### Verify\n", header(id, "accepted")),
        );
        path
    }

    fn git(dir: &Path, args: &[&str]) {
        std::process::Command::new("git")
            .args(["-c", "user.email=a@b.c", "-c", "user.name=a"])
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
    }

    // ── discover ──

    #[test]
    fn discover_lists_states_with_relative_paths() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        assert!(render_discover(&block(run_discover(
            true,
            None,
            &AdrApiOptions::default(),
            root
        )))
        .contains("No ADRs under docs/adr/."));

        adr(root, "0007-use-postgres.md", NYGARD);
        adr(root, "README.md", "# ADRs\n");
        local_pair(root, "a.md", ID, &doc("Logging", "Log JSON."));
        let legacy = format!("{OTHER}-x.md");
        adr(root, &legacy, "Text.\n## Policies\n- Do it.\n");
        write(
            &root.join(".claude/rules").join(format!("{OTHER}.md")),
            &format!("<rule_activation id=\"{OTHER}\">\n"),
        );
        let report = block(run_discover(true, None, &AdrApiOptions::default(), root));
        let unmanaged = report
            .entries
            .iter()
            .find(|e| e.path == Path::new("docs/adr/0007-use-postgres.md"))
            .unwrap();
        assert_eq!(unmanaged.state, State::Unmanaged);
        let managed = report
            .entries
            .iter()
            .find(|e| e.state == State::Managed)
            .unwrap();
        assert_eq!(
            managed.rule_file.as_deref(),
            Some(Path::new(".actual/rules/rule-a.md"))
        );
        assert_eq!(report.counts["unmanaged"], 1);
        assert_eq!(report.counts["legacy"], 1);
        assert!(report
            .cloud
            .note
            .as_deref()
            .unwrap()
            .contains("No synced decisions"));
        let text = render_discover(&report);
        assert!(text.contains("1 file to normalize"), "{text}");
        assert!(
            text.contains(&format!("beside it: .claude/rules/{OTHER}.md")),
            "{text}"
        );
        assert!(text.contains("Use Postgres"), "{text}");

        let json: serde_json::Value = serde_json::from_str(&json_text(&report)).unwrap();
        assert_eq!(json["entries"][0]["action"], "normalize");

        std::fs::remove_file(root.join(DECISION_DIR).join("0007-use-postgres.md")).unwrap();
        local_pair(root, "b.md", OTHER, &doc("More", "Do more."));
        let report = block(run_discover(true, None, &AdrApiOptions::default(), root));
        assert!(render_discover(&report).contains("Nothing to normalize."));
        let mut two = report.clone();
        two.entries.push(Entry {
            action: discovery::Action::Normalize,
            ..report.entries[0].clone()
        });
        two.entries.push(two.entries.last().unwrap().clone());
        assert!(render_discover(&two).contains("2 files to normalize"));
    }

    #[test]
    fn discover_marks_uncommitted_edits_to_synced_docs() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        git_repo(root, None);
        let doc_path = synced_pair(root, ID, "Use Postgres");
        synced_pair(root, OTHER, "Logging");
        git(root, &["add", "-A"]);
        git(root, &["commit", "-qm", "sync"]);
        let text = std::fs::read_to_string(&doc_path).unwrap();
        write(&doc_path, &text.replace("- Why.", "- Edited here."));
        let report = block(run_discover(true, None, &AdrApiOptions::default(), root));
        let edited = report
            .entries
            .iter()
            .find(|e| e.adr_id.as_deref() == Some(ID))
            .unwrap();
        assert_eq!(edited.state, State::SyncedEdited);
        assert!(
            edited.reason.contains(&format!("actual adr update {ID}")),
            "{}",
            edited.reason
        );
        let clean = report
            .entries
            .iter()
            .find(|e| e.adr_id.as_deref() == Some(OTHER))
            .unwrap();
        assert_eq!(clean.state, State::Synced);
        assert_eq!(
            report.cloud.note.as_deref(),
            Some("Skipped with --no-cloud.")
        );
        assert!(uncommitted(root, &[]).is_empty());
    }

    #[test]
    fn discover_marks_uncommitted_edits_below_the_git_top_level() {
        let dir = tempdir().unwrap();
        let top = dir.path();
        git_repo(top, None);
        let root = top.join("pkg");
        let edited = synced_pair(&root, ID, "Use Postgres");
        let moved = synced_pair(&root, OTHER, "Logging");
        synced_pair(&root, THIRD, "Tracing");
        git(top, &["add", "-A"]);
        git(top, &["commit", "-qm", "sync"]);
        let text = std::fs::read_to_string(&edited).unwrap();
        write(&edited, &text.replace("- Why.", "- Edited here."));
        // A rename's record is followed by the path it came from.
        let to = moved.with_file_name(format!("{OTHER}-moved.md"));
        git(top, &["mv", moved.to_str().unwrap(), to.to_str().unwrap()]);

        let report = block(run_discover(true, None, &AdrApiOptions::default(), &root));
        let state = |id: &str| {
            report
                .entries
                .iter()
                .find(|e| e.adr_id.as_deref() == Some(id))
                .map(|e| e.state)
                .unwrap()
        };
        assert_eq!(state(ID), State::SyncedEdited);
        assert_eq!(state(OTHER), State::SyncedEdited);
        assert_eq!(state(THIRD), State::Synced);
    }

    #[test]
    fn discover_compares_synced_docs_with_actual_ai() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let config = tempdir().unwrap();
        let _guards = sign_in(config.path());
        let dir = tempdir().unwrap();
        let root = dir.path();
        synced_pair(root, ID, "Use Postgres");
        synced_pair(root, OTHER, "Not in the cloud");
        let mut server = mockito::Server::new();
        server
            .mock("GET", "/v1/gcp/product/decisions")
            .match_query(Matcher::UrlEncoded("repository_id".into(), REPO.into()))
            .with_status(200)
            .with_body(format!(
                r#"{{"adrs":[{{"id":"{ID}","title":"Use MySQL","status":"accepted","policyRules":[{{"id":"r","adrId":"{ID}","level":"MUST","statement":"Store data in Postgres."}}]}}],"suggestions":[]}}"#
            ))
            .create();
        let api = AdrApiOptions {
            api_url: Some(server.url()),
            json: false,
        };
        let report = block(run_discover(false, Some(REPO), &api, root));
        assert!(report.cloud.compared);
        let edited = report
            .entries
            .iter()
            .find(|e| e.adr_id.as_deref() == Some(ID))
            .unwrap();
        assert_eq!(edited.state, State::SyncedEdited);
        assert!(
            edited.reason.starts_with("Its title differ"),
            "{}",
            edited.reason
        );
        let local = report
            .entries
            .iter()
            .find(|e| e.adr_id.as_deref() == Some(OTHER))
            .unwrap();
        assert_eq!(local.state, State::Synced);
        assert_eq!(local.action, discovery::Action::None);
        assert!(
            local.reason.starts_with("Not among the decisions"),
            "{}",
            local.reason
        );
    }

    #[test]
    fn compare_with_cloud_skips_groups_and_docless_entries() {
        let entry = |id: &str, title: &str, statement: &str| Entry {
            path: PathBuf::from(id),
            state: State::Synced,
            action: discovery::Action::None,
            adr_id: Some(id.to_string()),
            title: None,
            rule_file: None,
            reason: String::new(),
            duplicate_of: None,
            legacy_files: Vec::new(),
            doc: Some(doc(title, statement)),
            group_id: None,
        };
        let mut entries = vec![
            entry("same", "T", "Do it."),
            entry("policy", "T", "Do it."),
            entry("missing", "T", "Do it."),
            Entry {
                group_id: Some("g".to_string()),
                ..entry("grouped", "T", "Other.")
            },
            Entry {
                doc: None,
                ..entry("docless", "T", "Other.")
            },
        ];
        let cloud = |id: &str, statement: &str| ExistingDecision {
            id: id.to_string(),
            title: "t".to_string(),
            policies: vec![format!("  {statement} ")],
        };
        compare_with_cloud(
            &mut entries,
            &[
                cloud("same", "do  IT."),
                cloud("policy", "Do something else."),
                cloud("grouped", "Nope."),
                cloud("docless", "Nope."),
            ],
        );
        let states: Vec<State> = entries.iter().map(|e| e.state).collect();
        assert_eq!(
            states,
            [
                State::Synced,
                State::SyncedEdited,
                State::Synced,
                State::Synced,
                State::Synced
            ]
        );
        assert!(entries[1].reason.starts_with("Its policies differ"));
        assert_eq!(push_hint(None), "Save the edit to Actual AI with `actual adr update <id>` (or in the web app), or revert it.");
    }

    // ── normalize ──

    #[test]
    fn normalize_all_converts_reindexes_and_asks_a_model_for_prose() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        write(&root.join("src/lib.rs"), "");
        adr(root, "0007-use-postgres.md", NYGARD);
        adr(root, "0008-cache.md", PROSE);
        let stale = local_pair(root, "a.md", ID, &doc("Logging", "Log JSON."));
        let text = std::fs::read_to_string(&stale).unwrap();
        write(&stale, &text.replace("Log JSON.", "Log JSON lines."));
        local_pair(root, "b.md", OTHER, &doc("More", "Do more."));

        let report = run(&args(&[]), root, None, Some(Ok(model_answer()))).unwrap();
        assert_eq!(report.model.as_deref(), Some("fake"));
        assert_eq!(report.files.len(), 3, "{report:#?}");
        let nygard = file(&report, "0007-use-postgres.md");
        assert_eq!(nygard.outcome, Outcome::Written);
        assert_eq!(nygard.status.as_deref(), Some("accepted"));
        assert!(nygard.follow_ups.is_empty());
        let prose = file(&report, "0008-cache.md");
        assert_eq!(prose.outcome, Outcome::Written);
        assert!(prose.follow_ups[0].contains("A model wrote its policies"));
        assert_eq!(file(&report, "a.md").outcome, Outcome::Reindexed);

        let entries = discovery::discover(root);
        assert!(
            entries.iter().all(|e| e.state == State::Managed),
            "{entries:#?}"
        );
        let cache = files::read_decision_file(&root.join("docs/adr/0008-cache.md")).unwrap();
        assert_eq!(cache.meta.unwrap().paths, ["src/**"]);
        let text = render_normalize(&report);
        assert!(text.contains("normalized (accepted)"), "{text}");
        assert!(text.contains("re-indexed (accepted)"), "{text}");
        assert!(text.contains("Model: fake"), "{text}");

        // Run again: nothing left to do.
        let again = run(&args(&[]), root, None, None).unwrap();
        assert!(again.files.is_empty());
        assert!(render_normalize(&again).contains("Nothing to normalize."));
    }

    #[test]
    fn a_draft_whose_rule_file_was_deleted_is_reindexed() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        adr(
            root,
            "0009-logging.md",
            &NYGARD.replace("\nAccepted\n", "\nProposed\n"),
        );
        let first = run(&args(&[]), root, None, None).unwrap();
        let written = file(&first, "0009-logging.md");
        assert_eq!(written.status.as_deref(), Some("draft"));
        let rule = root.join(written.rule_file.as_deref().unwrap());
        std::fs::remove_file(&rule).unwrap();

        let entries = discovery::discover(root);
        assert_eq!(entries[0].state, State::MissingRule, "{entries:#?}");
        let again = run(&args(&[]), root, None, None).unwrap();
        assert_eq!(file(&again, "0009-logging.md").outcome, Outcome::Reindexed);
        assert!(rule.exists());
        assert_eq!(discovery::discover(root)[0].state, State::Managed);
    }

    #[test]
    fn dry_run_writes_nothing_and_shows_the_diff() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        adr(root, "0007-use-postgres.md", NYGARD);
        let stale = local_pair(root, "a.md", ID, &doc("Logging", "Log JSON."));
        let text = std::fs::read_to_string(&stale).unwrap();
        write(&stale, &text.replace("Log JSON.", "Log JSON lines."));
        let rule_before = std::fs::read_to_string(root.join(RULES_DIR).join("rule-a.md")).unwrap();
        let mut a = args(&[]);
        a.dry_run = true;
        let report = run(&a, root, None, None).unwrap();
        assert!(report.dry_run);
        let nygard = file(&report, "0007-use-postgres.md");
        assert_eq!(nygard.outcome, Outcome::WouldWrite);
        assert!(nygard.diff.as_deref().unwrap().contains("+version: 2"));
        assert_eq!(file(&report, "a.md").outcome, Outcome::WouldReindex);
        assert_eq!(
            std::fs::read_to_string(root.join("docs/adr/0007-use-postgres.md")).unwrap(),
            NYGARD
        );
        assert_eq!(
            std::fs::read_to_string(root.join(RULES_DIR).join("rule-a.md")).unwrap(),
            rule_before
        );
        let text = render_normalize(&report);
        assert!(text.contains("would normalize"), "{text}");
        assert!(text.contains("would re-index"), "{text}");
        assert!(text.contains("+version: 2"), "{text}");
    }

    #[test]
    fn named_files_in_other_states_are_skipped_or_unchanged() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        local_pair(root, "a.md", ID, &doc("Logging", "Log JSON."));
        synced_pair(root, OTHER, "Synced");
        adr(root, "README.md", "# ADRs\n");
        let legacy = format!("{ID}-x.md").replace(ID, "dddddddd-0000-4000-8000-000000000003");
        adr(root, &legacy, "Text.\n## Policies\n- Do it.\n");
        write(
            &root.join(".claude/rules/dddddddd-0000-4000-8000-000000000003.md"),
            "<rule_activation id=\"dddddddd-0000-4000-8000-000000000003\">\n",
        );
        let synced = format!("docs/adr/{OTHER}-synced.md");
        let legacy_rel = format!("docs/adr/{legacy}");
        let abs = root.join("docs/adr/README.md");
        let named = args(&["docs/adr/a.md", &synced, &legacy_rel, abs.to_str().unwrap()]);
        let report = run(&named, root, None, None).unwrap();
        assert_eq!(file(&report, "a.md").outcome, Outcome::Unchanged);
        let synced = file(&report, "-synced.md");
        assert_eq!(synced.outcome, Outcome::Skipped);
        assert!(synced.reason.as_deref().unwrap().contains("sync owns"));
        assert!(file(&report, &legacy)
            .reason
            .as_deref()
            .unwrap()
            .contains("--legacy"));
        assert_eq!(file(&report, "README.md").outcome, Outcome::Skipped);
        assert!(render_normalize(&report).contains("skipped"));

        let err = run(&args(&["docs/adr/nope.md"]), root, None, None).unwrap_err();
        assert!(
            err.to_string().contains("isn't a markdown file under"),
            "{err}"
        );
    }

    #[test]
    fn without_a_model_prose_needs_answers() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        adr(root, "0008-cache.md", PROSE);
        let report = run(&args(&[]), root, None, None).unwrap();
        assert_eq!(
            report.model.as_deref(),
            Some("No model ran: no runner here")
        );
        let cache = file(&report, "0008-cache.md");
        assert_eq!(cache.outcome, Outcome::NeedsInput);
        assert_eq!(cache.questions[0].field, "policies");
        let text = render_normalize(&report);
        assert!(text.contains("needs answers"), "{text}");
        assert!(text.contains("[policies]"), "{text}");
        assert!(text.contains("Some files need answers"), "{text}");

        // A model that fails leaves the same question, and says why.
        let report = run(&args(&[]), root, None, Some(Err("boom".to_string()))).unwrap();
        assert!(report.model.as_deref().unwrap().contains("boom"));
        assert_eq!(file(&report, "0008-cache.md").outcome, Outcome::NeedsInput);

        // --no-model asks without trying.
        let mut no_model = args(&[]);
        no_model.no_model = true;
        let report = run(&no_model, root, None, Some(Ok(model_answer()))).unwrap();
        assert_eq!(report.model, None);
        // A suggested answer is printed with its question.
        let mut shown = report.clone();
        shown.files[0].questions[0].suggested = vec!["MUST: x".to_string()];
        assert!(render_normalize(&shown).contains("suggested: MUST: x"));
    }

    #[test]
    fn extractions_and_answers_come_from_files() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        adr(root, "0008-cache.md", PROSE);
        let extracted = root.join("extracted.json");
        write(
            &extracted,
            r#"{"docs/adr/0008-cache.md": {"policies": ["MUST: Sessions are cached in Redis."]}}"#,
        );
        let answers = root.join("answers.json");
        let abs = root.join("docs/adr/0008-cache.md");
        write(
            &answers,
            &serde_json::json!({ abs.display().to_string(): {"status": "draft"} }).to_string(),
        );
        let mut a = args(&[]);
        a.extracted = Some(extracted);
        a.answers = Some(answers.clone());
        let report = run(&a, root, None, None).unwrap();
        assert_eq!(report.model, None);
        let cache = file(&report, "0008-cache.md");
        assert_eq!(cache.outcome, Outcome::Written, "{cache:#?}");
        assert_eq!(cache.status.as_deref(), Some("draft"));

        write(&answers, "[1]");
        let err = run(&a, root, None, None).unwrap_err();
        assert!(err.to_string().contains("keyed by path"), "{err}");
        a.answers = Some(root.join("missing.json"));
        assert!(run(&a, root, None, None).is_err());
    }

    #[test]
    fn yes_takes_safe_answers() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        adr(
            root,
            "0009-modules.md",
            "# Modules\n\n## Policies\n\n- Keep modules small.\n",
        );
        adr(root, "0010-prose.md", PROSE);
        let mut a = args(&[]);
        a.yes = true;
        a.no_model = true;
        let report = run(&a, root, None, None).unwrap();
        let modules = file(&report, "0009-modules.md");
        assert_eq!(modules.status.as_deref(), Some("draft"));
        assert_eq!(modules.defaulted.len(), 1);
        assert!(render_normalize(&report).contains("note:"));
        let prose = file(&report, "0010-prose.md");
        assert_eq!(prose.outcome, Outcome::Skipped);
    }

    #[test]
    fn legacy_files_convert_on_request_and_rename_when_asked() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        let name = format!("{ID}-adopt-modules.md");
        adr(root, &name, "# Adopt Modular Libraries\n\nStatus: accepted\n\n## Policies\n\n- Keep modules small.\n");
        write(
            &root.join(".claude/rules").join(format!("{ID}.md")),
            &format!("<rule_activation id=\"{ID}\">\n"),
        );
        let target = decision_doc_file_name(ID, "Adopt Modular Libraries");
        // The renamed doc's name is taken: left alone.
        adr(root, &target, "taken");
        let mut a = args(&[]);
        a.legacy = true;
        a.rename = true;
        let report = run(&a, root, None, None).unwrap();
        let skipped = file(&report, &name);
        assert_eq!(skipped.outcome, Outcome::Skipped, "{report:#?}");
        assert!(skipped
            .reason
            .as_deref()
            .unwrap()
            .contains("already exists"));

        std::fs::remove_file(root.join(DECISION_DIR).join(&target)).unwrap();
        let report = run(&a, root, None, None).unwrap();
        let moved = file(&report, &name);
        assert_eq!(moved.outcome, Outcome::Written);
        assert_eq!(moved.adr_id.as_deref(), Some(ID));
        assert_eq!(
            moved.decision_file.as_deref(),
            Some(format!("docs/adr/{target}").as_str())
        );
        assert!(moved.follow_ups[0].contains("actual adr-bot"));
        assert!(!root.join(DECISION_DIR).join(&name).exists());
        assert!(root.join(DECISION_DIR).join(&target).exists());
        assert!(render_normalize(&report).contains(&format!("-> docs/adr/{target}")));
    }

    #[test]
    fn a_person_answers_at_the_terminal() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        write(&root.join("src/lib.rs"), "");
        adr(
            root,
            "0011-undecided.md",
            "# Undecided\n\n## Decision\n\nWe might do it.\n",
        );
        let term =
            MockTerminal::new(vec!["MUST: Do it.", "", "src/**"]).with_select_one(vec![0, 2]);
        let mut a = args(&[]);
        a.no_model = true;
        let report = run(&a, root, Some(&term), None).unwrap();
        let undecided = file(&report, "0011-undecided.md");
        assert_eq!(undecided.outcome, Outcome::Written, "{undecided:#?}");
        assert_eq!(undecided.status.as_deref(), Some("accepted"));
        assert!(term.output_text().contains("docs/adr/0011-undecided.md"));

        // Answers that never settle stop after three rounds.
        adr(root, "0012-more.md", "# More\n\n## Decision\n\nWe might.\n");
        let term = MockTerminal::new(vec!["nope", "", "nope", "", "nope", ""])
            .with_select_one(vec![0, 0, 0]);
        let report = run(&args(&["docs/adr/0012-more.md"]), root, Some(&term), None).unwrap();
        assert_eq!(file(&report, "0012-more.md").outcome, Outcome::NeedsInput);
        // The --yes tip comes once a run, before the first question.
        let said = term.output_text();
        assert_eq!(said.matches(YES_HINT).count(), 1, "{said}");
        assert!(said.find(YES_HINT) < said.find("docs/adr/0012-more.md"));
    }

    #[test]
    fn progress_takes_any_runner_label() {
        let progress = Progress::new(true);
        progress.finding_model(1);
        progress.reading(2, "odd {runner}");
        progress.file("docs/adr/a.md");
        progress.done_one();
        progress.finish();
        assert!(progress.0.is_finished());
    }

    #[test]
    fn ask_questions_covers_every_field() {
        let q = |field: &'static str, options: &[&str], suggested: &[&str]| Question {
            field,
            question: format!("{field}?"),
            options: options.iter().map(|o| o.to_string()).collect(),
            suggested: suggested.iter().map(|s| s.to_string()).collect(),
        };
        let questions = [
            q("keep_duplicate", &["true", "false"], &[]),
            q("is_adr", &["true", "false"], &[]),
            q("status", &["accepted", "draft"], &[]),
            q("policies", &[], &["MUST: x"]),
            q("scope", &[], &["src/**"]),
        ];
        let term = MockTerminal::new(vec!["y", "n"]).with_select_one(vec![1, 0, 1]);
        let answers = ask_questions(&term, &questions).unwrap();
        assert_eq!(answers.keep_duplicate, Some(true));
        assert_eq!(answers.is_adr, Some(false));
        assert_eq!(answers.status.as_deref(), Some("draft"));
        assert_eq!(
            answers.policies.as_deref(),
            Some(&["MUST: x".to_string()][..])
        );
        assert_eq!(answers.scope.as_deref(), Some(&["src/**".to_string()][..]));
        assert!(term.output_text().contains("  MUST: x"));

        // Enter my own policies; enter paths when there is a suggestion.
        let term =
            MockTerminal::new(vec!["SHOULD: y", " ", "a/**, , b/**"]).with_select_one(vec![1, 2]);
        let answers = ask_questions(&term, &questions[3..]).unwrap();
        assert_eq!(
            answers.policies.as_deref(),
            Some(&["SHOULD: y".to_string()][..])
        );
        assert_eq!(
            answers.scope.as_deref(),
            Some(&["a/**".to_string(), "b/**".to_string()][..])
        );
        // The whole repository.
        let term = MockTerminal::new(vec![]).with_select_one(vec![0]);
        assert_eq!(
            ask_questions(&term, &questions[4..]).unwrap().scope,
            Some(Vec::new())
        );
    }

    #[test]
    fn possible_duplicates_and_stale_docs_over_the_cap() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        synced_pair(root, ID, "Use Postgres for storage");
        adr(
            root,
            "0007-dup.md",
            "# Use Postgres for storage\n\nStatus: accepted\n\n## Decision\n\n- MUST: Store data in Postgres.\n",
        );
        let big = context_file::render(&ContextFile {
            context: vec!["x".repeat(110_000)],
            ..doc("Huge", "Do it.")
        });
        adr(
            root,
            "huge.md",
            &format!("{}\n{big}", header(OTHER, "accepted")),
        );
        let mut a = args(&[]);
        a.yes = true;
        let report = run(&a, root, None, None).unwrap();
        let dup = file(&report, "0007-dup.md");
        assert_eq!(dup.state, State::PossibleDuplicate);
        assert_eq!(dup.outcome, Outcome::Skipped);
        let huge = file(&report, "huge.md");
        assert_eq!(huge.state, State::MissingRule);
        assert!(
            huge.reason.as_deref().unwrap().contains("too long"),
            "{huge:#?}"
        );
        a.allow_duplicate = true;
        let report = run(&a, root, None, None).unwrap();
        assert_eq!(file(&report, "0007-dup.md").outcome, Outcome::Written);

        // A headed doc of ordinary size with no rule file gets one.
        let id = "eeeeeeee-0000-4000-8000-000000000004";
        let body = context_file::render(&doc("Small", "Do it."));
        adr(
            root,
            "small.md",
            &format!("{}\n{body}", header(id, "accepted")),
        );
        let report = run(&args(&["docs/adr/small.md"]), root, None, None).unwrap();
        let small = file(&report, "small.md");
        assert_eq!(small.outcome, Outcome::Reindexed);
        assert!(root.join(small.rule_file.as_deref().unwrap()).exists());
    }

    #[test]
    fn push_saves_what_was_written() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let config = tempdir().unwrap();
        let _guards = sign_in(config.path());
        let dir = tempdir().unwrap();
        let root = dir.path();
        git_repo(root, Some("https://github.com/actual-software/actual-cli"));
        adr(
            root,
            "0007-use-postgres.md",
            NYGARD.replace("Accepted", "Proposed").as_str(),
        );
        adr(
            root,
            "0009-old.md",
            &NYGARD
                .replace("Accepted", "Superseded")
                .replace("Postgres", "Oracle"),
        );
        let mut server = mockito::Server::new();
        server
            .mock("GET", "/v1/connected-repos")
            .match_query(Matcher::Any)
            .with_status(200)
            .with_body(CONNECTED)
            .create();
        server
            .mock("GET", "/v1/gcp/product/decisions")
            .match_query(Matcher::Any)
            .with_status(200)
            .with_body(r#"{"adrs":[],"suggestions":[],"version":1,"repository_summaries":[]}"#)
            .create();
        let import = server
            .mock("POST", "/v1/gcp/product/adr-import")
            .with_status(200)
            .with_body(r#"{"success":true,"adr_id":"33333333-3333-4333-8333-333333333333"}"#)
            .expect(1)
            .create();
        let mut a = args(&[]);
        a.push = true;
        a.api.api_url = Some(server.url());
        let report = run(&a, root, None, None).unwrap();
        import.assert();
        let pushed = file(&report, "0007-use-postgres.md");
        assert!(pushed.pushed.is_some(), "{pushed:#?}");
        let old = file(&report, "0009-old.md");
        assert!(old.push_problem.as_deref().unwrap().contains("superseded"));
        let text = render_normalize(&report);
        assert!(text.contains("saved to Actual AI"), "{text}");
        assert!(text.contains("push:"), "{text}");

        // Signed out, the push is a problem and the files stay written.
        let _out = EnvGuard::set(
            "ACTUAL_CONFIG_DIR",
            tempdir().unwrap().path().to_str().unwrap(),
        );
        adr(root, "0010-new.md", &NYGARD.replace("Postgres", "Redis"));
        let report = run(&args(&["docs/adr/0010-new.md"]), root, None, None).unwrap();
        assert!(file(&report, "0010-new.md").push_problem.is_none());
        adr(root, "0011-new.md", &NYGARD.replace("Postgres", "Kafka"));
        let mut a = args(&["docs/adr/0011-new.md"]);
        a.push = true;
        a.allow_duplicate = true;
        let report = run(&a, root, None, None).unwrap();
        assert!(file(&report, "0011-new.md").push_problem.is_some());
        let err = unsaved_error(&report).expect("a push problem fails the run");
        assert_eq!(err.exit_code(), 3);
        assert!(err.to_string().contains("written here but not saved"));
        let mut clean = report.clone();
        clean.files.retain(|f| f.push_problem.is_none());
        assert!(unsaved_error(&clean).is_none());
    }

    /// A pair whose header declares `paths` from `scope_root`.
    fn scoped_pair(root: &Path, name: &str, id: &str, scope_root: &str, paths: &[&str]) -> PathBuf {
        let doc = doc("Charge cards", "Charge cards through Stripe.");
        let paths: Vec<String> = paths.iter().map(|p| p.to_string()).collect();
        let decision = Decision {
            doc: &doc,
            adr_id: id,
            status: "accepted",
            root: scope_root,
            scope: &paths,
        };
        let path = adr(root, name, &decision.doc_text(name, &[]));
        write(
            &root.join(RULES_DIR).join(format!("rule-{name}")),
            &decision.rule_text(&format!("rule-{name}"), &format!("docs/adr/{name}")),
        );
        path
    }

    #[test]
    fn a_decision_written_again_keeps_the_scope_it_declares() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        write(&root.join("services/payments/charge.rs"), "");
        write(&root.join("services/api/src/main.rs"), "");
        // Its body lost its policy; its header still names its scope.
        let kept = scoped_pair(root, "a.md", ID, ".", &["services/payments/**"]);
        let text = std::fs::read_to_string(&kept).unwrap();
        write(&kept, &text.replace("Charge cards through Stripe.", ""));
        // Its header is gone; its rule file still names its scope.
        let gone = scoped_pair(root, "b.md", OTHER, "services/api", &["src/**"]);
        write(&gone, "# Serve the API\n\nWe serve it from one binary.\n");
        let states: Vec<State> = discovery::discover(root).iter().map(|e| e.state).collect();
        assert_eq!(states, [State::Broken, State::Broken]);

        let answers = root.join("answers.json");
        write(
            &answers,
            r#"{"docs/adr/a.md": {"policies": ["MUST: Charge cards through Stripe."]},
                "docs/adr/b.md": {"policies": ["MUST: Serve the API from one binary."],
                                  "status": "accepted"}}"#,
        );
        let mut a = args(&[]);
        a.answers = Some(answers);
        let report = run(&a, root, None, None).unwrap();
        for (name, scope) in [
            ("a.md", "services/payments/**"),
            ("b.md", "services/api/src/**"),
        ] {
            let written = file(&report, name);
            assert_eq!(written.outcome, Outcome::Written, "{written:#?}");
            assert_eq!(written.status.as_deref(), Some("accepted"));
            let rule =
                std::fs::read_to_string(root.join(written.rule_file.as_deref().unwrap())).unwrap();
            assert!(rule.contains(&format!("- {scope}")), "{rule}");
        }
        let entries = discovery::discover(root);
        assert!(
            entries.iter().all(|e| e.state == State::Managed),
            "{entries:#?}"
        );
    }

    #[test]
    fn a_model_does_not_replace_a_declared_scope() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        write(&root.join("services/payments/charge.rs"), "");
        write(&root.join("src/lib.rs"), "");
        let kept = scoped_pair(root, "a.md", ID, ".", &["services/payments/**"]);
        let text = std::fs::read_to_string(&kept).unwrap();
        write(
            &kept,
            &text.replace("1. MUST: Charge cards through Stripe.\n", ""),
        );
        let report = run(&args(&[]), root, None, Some(Ok(model_answer()))).unwrap();
        let written = file(&report, "a.md");
        assert_eq!(written.outcome, Outcome::Written, "{written:#?}");
        let rule =
            std::fs::read_to_string(root.join(written.rule_file.as_deref().unwrap())).unwrap();
        assert!(rule.contains("- services/payments/**"), "{rule}");
        assert!(!rule.contains("- src/**"), "{rule}");
    }

    #[test]
    fn a_scope_that_cannot_be_read_is_asked_about() {
        let dir = tempdir().unwrap();
        let mut found = Extraction::default();
        let mut entry = Entry::new(dir.path().join("gone.md"), State::Broken, "");
        keep_declared_scope(&entry, &mut found);
        assert_eq!(found.scope, None);
        assert!(found.is_uncertain("scope"));
        let mut unmanaged = Extraction::default();
        entry.state = State::Unmanaged;
        keep_declared_scope(&entry, &mut unmanaged);
        assert_eq!(unmanaged, Extraction::default());
    }

    #[test]
    fn a_reindex_keeps_a_package_root() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        write(&root.join("services/api/src/main.rs"), "");
        scoped_pair(root, "a.md", ID, "services/api", &["src/**"]);
        std::fs::remove_file(root.join(RULES_DIR).join("rule-a.md")).unwrap();
        assert_eq!(discovery::discover(root)[0].state, State::MissingRule);

        let report = run(&args(&[]), root, None, None).unwrap();
        let reindexed = file(&report, "a.md");
        assert_eq!(reindexed.outcome, Outcome::Reindexed, "{reindexed:#?}");
        let rule =
            std::fs::read_to_string(root.join(reindexed.rule_file.as_deref().unwrap())).unwrap();
        assert!(rule.contains("root: services/api"), "{rule}");
        assert!(rule.contains("services/api/src/**"), "{rule}");
        assert_eq!(discovery::discover(root)[0].state, State::Managed);
    }

    #[test]
    fn repo_globs_join_a_header_root() {
        let globs = |root: &str, paths: &[&str]| {
            let paths: Vec<String> = paths.iter().map(|p| p.to_string()).collect();
            repo_globs(root, &paths)
        };
        assert_eq!(globs("./", &["src/**"]), ["src/**"]);
        assert_eq!(globs(".", &[]), Vec::<String>::new());
        assert_eq!(globs("pkg/", &[]), ["pkg/**"]);
        assert_eq!(
            globs("pkg", &["./src/**", "!src/gen/**"]),
            ["pkg/src/**", "!pkg/src/gen/**"]
        );
        assert_eq!(globs("pkg", &["!gen/**"]), ["pkg/**", "!pkg/gen/**"]);
    }

    #[test]
    fn repo_files_match_globs_and_directories() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        write(&root.join("src/api/main.rs"), "");
        write(&root.join("docs/x.md"), "");
        let files = RepoFiles::new(root);
        assert!(files.matches("src/**/*.rs"));
        assert!(files.matches("./src/api"));
        assert!(files.matches("src/**"));
        assert!(!files.matches("lib/**"));
        assert!(!files.matches("[")); // not a pattern, and no such directory
        assert_eq!(files.top_dirs(), ["docs", "src"]);
    }

    #[test]
    fn repo_files_match_brace_lists_and_hidden_files() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        write(&root.join("src/web/app.tsx"), "");
        write(&root.join(".github/workflows/ci.yml"), "");
        write(&root.join(".git/config"), "");
        let files = RepoFiles::new(root);
        assert!(files.matches("src/**/*.{ts,tsx}"));
        assert!(files.matches(".github/workflows/**"));
        assert!(!files.matches(".git/**"));
        assert!(!files.matches("src/**/*.{rs,go}"));
        assert_eq!(files.top_dirs(), ["src"]);

        // A listing cut short can't say a glob matches nothing.
        let cut = RepoFiles {
            limit: 1,
            ..RepoFiles::new(root)
        };
        assert!(cut.matches("lib/**"));
    }

    #[test]
    fn exec_commands_print_and_resolve_a_runner() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _key = EnvGuard::remove("ANTHROPIC_API_KEY");
        let config = tempdir().unwrap();
        let _cfg = EnvGuard::set("ACTUAL_CONFIG_DIR", config.path().to_str().unwrap());
        let dir = tempdir().unwrap();
        let root = dir.path();
        adr(root, "0008-cache.md", PROSE);
        for json in [false, true] {
            exec_discover(&AdrDiscoverArgs {
                no_cloud: true,
                repo: None,
                root: Some(root.to_path_buf()),
                api: AdrApiOptions {
                    api_url: None,
                    json,
                },
            })
            .unwrap();
            let mut a = args(&[]);
            a.root = Some(root.to_path_buf());
            a.runner = Some(crate::cli::args::RunnerChoice::AnthropicApi);
            a.api.json = json;
            exec_normalize(&a).unwrap();
        }
        let resolved = rules_rank::ResolvedRunner {
            runner: rules_rank::SelectionRunner::ClaudeCli(
                crate::runner::subprocess::CliClaudeRunner::new(
                    PathBuf::from("/nonexistent/claude"),
                    std::time::Duration::from_secs(1),
                ),
            ),
            choice: crate::cli::args::RunnerChoice::ClaudeCli,
            model: Some("m".to_string()),
            max_budget_usd: None,
        };
        let model = model_from(resolved);
        assert_eq!(model.model.as_deref(), Some("m"));
        assert!(model.label.contains("m"));
    }
}
