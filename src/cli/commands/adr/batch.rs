//! The publishing runs. `actual adr create --all` is a person's way to send
//! every decision file under `docs/adr/` at once. `actual adr push` is the
//! step the plugin runs at the end of each turn: it sends only the files that
//! are new or changed, at most `--limit` of them, and when it can't publish
//! it publishes nothing and says nothing.
//!
//! Both follow one set of rules. A file is sent when it carries an ADR header
//! and passes `actual adr check`, and nothing under `docs/adr/drafts/` ever
//! is. A file whose id, in its header or its name, is one of the repository's
//! decisions is skipped and listed, and so is one whose status doesn't govern
//! and one that looks like a decision already there. Each save names the
//! file's path, so a second save of one file, from any checkout, returns the
//! decision the first made, and a draft takes the new text. A draft or
//! proposed file becomes a cloud draft and keeps its name. An accepted file
//! is accepted at once, and renamed to the decision's own names, only by
//! `create --all` with a person at the terminal. Any other run saves it as a
//! draft, and the first run after the file reaches the default branch sends
//! that branch's text and accepts the draft, so what governs is what the
//! merge reviewed; the sync then replaces the file.
//!
//! A run reads the repository's decisions once, then takes the files in path
//! order. It stops at the first save whose outcome is unknown, and at the
//! first error that isn't about the file being saved. [`super::record`] keeps
//! what each file last did.

use std::fmt::Write as _;
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use chrono::Utc;
use serde::Serialize;
use uuid::Uuid;

use super::cloud::{decisions, finish_file_save, CreateOutcome, Rename};
use super::local::in_drafts;
use super::record::{self, Entry, Key, Outcome, Record};
use super::{interactive, json_text, pick_repo, runtime, session, today, Repo, RepoPick, Session};
use crate::adr::context_file::{self, ContextFile};
use crate::adr::files::{self, DecisionFile, DECISION_DIR};
use crate::adr::overlap::{find_overlaps, ExistingDecision};
use crate::adr::save_check::check_markdown;
use crate::api::adr_types::{Decision, ImportRequest, ImportResponse, SaveStatus};
use crate::auth::store;
use crate::cli::args::{AdrApiOptions, AdrCreateArgs, AdrPushArgs};
use crate::cli::commands::advisor::{origin_remote_url, parse_git_remote_url};
use crate::cli::commands::sync::resolve_cwd;
use crate::cli::ui::theme;
use crate::error::{sanitize_server_text, ActualError};

/// How long one git read of the default branch may take.
const GIT_TIMEOUT: Duration = Duration::from_secs(5);

/// A file saved, with what `create --file` reports for it.
#[derive(Debug, Serialize)]
struct Saved {
    file: String,
    /// The file's path was saved before, so this is that save's decision.
    #[serde(skip_serializing_if = "is_false")]
    already_saved: bool,
    /// Saved as a draft from an accepted file, which a later run accepts
    /// once the file is on the default branch.
    #[serde(skip_serializing_if = "is_false")]
    waits_for_merge: bool,
    #[serde(flatten)]
    outcome: CreateOutcome,
}

fn is_false(value: &bool) -> bool {
    !value
}

/// A file the run left alone or couldn't save, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct FileNote {
    file: String,
    reason: String,
}

/// What a publishing run did, for `--json`.
#[derive(Debug, Default, Serialize)]
struct BatchReport {
    /// The connected repository the files went to, once one was chosen.
    #[serde(skip_serializing_if = "Option::is_none")]
    repo_unique_id: Option<String>,
    #[serde(skip)]
    repo_label: String,
    /// Decision files found under `docs/adr/`, unreadable ones included and
    /// `docs/adr/drafts/` left out.
    files: usize,
    saved: Vec<Saved>,
    /// Drafts saved earlier from accepted files whose file reached the
    /// default branch, each sent again with that branch's text and accepted.
    merged: Vec<Saved>,
    skipped: Vec<FileNote>,
    not_saved: Vec<FileNote>,
    /// The file the run stopped at, and why; `not_sent` lists the files
    /// after it.
    #[serde(skip_serializing_if = "Option::is_none")]
    stopped: Option<FileNote>,
    not_sent: Vec<String>,
    /// The files `push` left for its next run, past its limit.
    left_for_next_run: Vec<String>,
    /// Why `push` published nothing: no sign-in, a repository Actual AI
    /// hasn't connected, or a role that can't save decisions.
    #[serde(skip_serializing_if = "Option::is_none")]
    stood_down: Option<String>,
    /// Markdown under `docs/adr/` with no ADR header, which `create` can't
    /// send.
    other_files: Vec<String>,
}

pub(super) fn exec_create_all(args: &AdrCreateArgs) -> Result<(), ActualError> {
    let run = Run {
        interactive: interactive(),
        limit: None,
        allow_duplicate: args.allow_duplicate,
    };
    let target = Target {
        repo: args.repo.as_deref(),
        api: &args.api,
    };
    let root = resolve_cwd();
    let mut progress = std::io::stderr();
    let (report, stop) =
        runtime()?.block_on(run_publish(&target, &root, run, &today(), &mut progress))?;
    if args.api.json {
        println!("{}", json_text(&report));
    } else {
        print!("{}", render(&report));
    }
    finish(&report, stop)
}

pub(super) fn exec_push(args: &AdrPushArgs) -> Result<(), ActualError> {
    let root = args.root.clone().unwrap_or_else(resolve_cwd);
    let run = Run {
        interactive: false,
        limit: Some(usize::try_from(args.limit).unwrap_or(usize::MAX)),
        allow_duplicate: false,
    };
    let target = Target {
        repo: args.repo.as_deref(),
        api: &args.api,
    };
    let mut progress = std::io::sink();
    let (report, stop) =
        runtime()?.block_on(run_publish(&target, &root, run, &today(), &mut progress))?;
    if args.api.json {
        println!("{}", json_text(&report));
    } else {
        print!("{}", render_push(&report));
    }
    stop.map_or(Ok(()), Err)
}

/// `create --all`'s outcome: the error that stopped it, else a failure when
/// a file wasn't saved.
fn finish(report: &BatchReport, stop: Option<ActualError>) -> Result<(), ActualError> {
    match stop {
        Some(e) => Err(e),
        None if report.not_saved.is_empty() => Ok(()),
        None => Err(ActualError::AdrCheckFailed(format!(
            "{} of {} decision files weren't saved",
            report.not_saved.len(),
            report.files
        ))),
    }
}

/// How a run treats the files, as `create --all` or as `push`.
#[derive(Debug, Clone, Copy)]
struct Run {
    /// A person is at the terminal, so an accepted file is accepted at once.
    interactive: bool,
    /// `push`'s cap on the files one run sends. `None` is `create --all`,
    /// which reads every file again, sends any number and says why it can't
    /// publish.
    limit: Option<usize>,
    allow_duplicate: bool,
}

impl Run {
    fn is_push(self) -> bool {
        self.limit.is_some()
    }

    fn at_limit(self, sent: usize) -> bool {
        self.limit.is_some_and(|limit| sent >= limit)
    }
}

/// The repository flag and API options a run calls Actual AI with.
struct Target<'a> {
    repo: Option<&'a str>,
    api: &'a AdrApiOptions,
}

/// Why a file isn't sent.
enum Unsent {
    Skip(String),
    NotSaved(String),
}

/// A file to send: its path as the repository names it, the file, and its
/// whole text, which the record keeps a hash of.
struct Pending<'a> {
    name: String,
    file: &'a DecisionFile,
    text: String,
}

/// A draft saved from an accepted file, whose file is accepted on the
/// default branch and passes the check there: that copy, to send instead.
struct Merged {
    name: String,
    file: DecisionFile,
    doc: ContextFile,
    entry: Entry,
}

/// Where a run sends: the signed-in session, the repository, and the
/// repository's decisions as they were read once at the start.
struct Cloud {
    session: Session,
    repo: Repo,
    existing: Vec<Decision>,
    comparable: Vec<ExistingDecision>,
}

/// Why `push` publishes nothing, and whether the files it would have sent
/// wait [`record::Outcome::StoodDown`]'s hold before they are tried again.
struct StandDown {
    reason: String,
    hold: bool,
}

/// The run's record and where it is saved. A run with no repository to name
/// the record by keeps none.
struct Book {
    place: Option<(PathBuf, Key)>,
    record: Record,
}

impl Book {
    async fn open(org_id: &str, repo_flag: Option<&str>, root: &Path) -> Self {
        let origin = origin_remote_url(root).await;
        let repo = match origin.as_deref().and_then(parse_git_remote_url) {
            Some(remote) => Some(remote.slug()),
            None => repo_flag.map(str::to_string),
        };
        let place = record::dir().zip(repo.map(|repo| Key {
            org_id: org_id.to_string(),
            repo,
        }));
        let record = place
            .as_ref()
            .map(|(dir, key)| record::load(dir, key))
            .unwrap_or_default();
        Self { place, record }
    }

    fn entry(&self, name: &str) -> Option<&Entry> {
        self.record.files.get(name)
    }

    /// The decision `name` was saved as, which is never its own duplicate.
    fn own_id(&self, name: &str) -> Option<String> {
        self.entry(name).and_then(|entry| entry.adr_id.clone())
    }

    fn note(&mut self, name: &str, entry: Entry) {
        if let Some((dir, key)) = &self.place {
            record::save(dir, key, name, entry.clone());
        }
        self.record.files.insert(name.to_string(), entry);
    }

    /// Hold `pending` for the stand-down's while, keeping the decision each
    /// was saved as.
    fn hold(&mut self, pending: &[Pending<'_>]) {
        for item in pending {
            let own = self.own_id(&item.name);
            self.note(
                &item.name,
                Entry::new(&item.text, Outcome::StoodDown, own.as_deref()),
            );
        }
    }
}

/// Send the decision files under `root/docs/adr/` that the run takes,
/// writing a progress line per file. Returns the report and the error that
/// stopped the run partway, if one did; an error before the first file comes
/// back on its own.
async fn run_publish(
    target: &Target<'_>,
    root: &Path,
    run: Run,
    today: &str,
    progress: &mut dyn Write,
) -> Result<(BatchReport, Option<ActualError>), ActualError> {
    let found = files::discover(root);
    let outside_drafts = |path: &Path| !in_drafts(&source_path(root, path));
    let candidates: Vec<&DecisionFile> = found
        .decisions
        .iter()
        .filter(|file| outside_drafts(&file.path))
        .collect();
    let unreadable: Vec<FileNote> = found
        .unreadable
        .iter()
        .filter(|(path, _)| outside_drafts(path))
        .map(|(path, reason)| FileNote {
            file: source_path(root, path),
            reason: reason.clone(),
        })
        .collect();
    let mut report = BatchReport {
        files: candidates.len() + unreadable.len(),
        ..BatchReport::default()
    };
    if !run.is_push() {
        report.not_saved = unreadable;
        report.other_files = found
            .skipped
            .iter()
            .filter(|path| outside_drafts(path))
            .map(|path| source_path(root, path))
            .collect();
    }
    if candidates.is_empty() {
        return Ok((report, None));
    }

    // The record is the sign-in organization's, read from disk with no
    // request.
    let creds = match store::load() {
        Ok(Some(creds)) => creds,
        Ok(None) if run.is_push() => return Ok((stood_down(report, "Not signed in."), None)),
        Ok(None) => return Err(ActualError::NotLoggedIn),
        Err(e) if run.is_push() => {
            let reason = format!("The sign-in couldn't be read: {e}");
            return Ok((stood_down(report, &reason), None));
        }
        Err(e) => return Err(e),
    };
    let mut book = Book::open(&creds.organization_id, target.repo, root).await;
    let now = Utc::now();
    let pending: Vec<Pending<'_>> = candidates
        .into_iter()
        .filter_map(|file| {
            let name = source_path(root, &file.path);
            let text = std::fs::read_to_string(&file.path).ok()?;
            let settled = book.entry(&name).is_some_and(|entry| {
                entry.settles(&text, now)
                    || (entry.outcome == Outcome::WaitsForMerge
                        && entry.text == record::text_hash(&text))
            });
            (!run.is_push() || !settled).then_some(Pending { name, file, text })
        })
        .collect();
    let merged = merged_copies(&book, root, today).await;
    if pending.is_empty() && merged.is_empty() {
        return Ok((report, None));
    }

    let cloud = match connect(target, root, run).await? {
        Ok(cloud) => cloud,
        Err(stand_down) => {
            if stand_down.hold {
                book.hold(&pending);
            }
            return Ok((stood_down(report, &stand_down.reason), None));
        }
    };
    report.repo_unique_id = Some(cloud.repo.id.clone());
    report.repo_label = cloud.repo.label.clone();
    let mut publishing = Publishing {
        run,
        root,
        today,
        progress,
        cloud,
        book,
        report,
        sent: 0,
    };
    for item in merged {
        let name = item.name.clone();
        match publishing.accept_merged(item).await {
            Next::Go => {}
            Next::StandDown(stand_down) => return Ok(publishing.stand_down(stand_down, &pending)),
            Next::Stop(e) => {
                let rest = pending.iter().map(|item| item.name.clone()).collect();
                return Ok(publishing.stop(name, e, rest));
            }
        }
    }
    let total = pending.len();
    for (index, item) in pending.iter().enumerate() {
        let step = format!("[{}/{total}] {}", index + 1, item.name);
        match publishing.send(&step, item).await {
            Next::Go => {}
            Next::StandDown(stand_down) => {
                return Ok(publishing.stand_down(stand_down, &pending[index..]))
            }
            Next::Stop(e) => {
                let rest = pending[index + 1..]
                    .iter()
                    .map(|item| item.name.clone())
                    .collect();
                return Ok(publishing.stop(item.name.clone(), e, rest));
            }
        }
    }
    Ok((publishing.report, None))
}

/// `report` with `push`'s reason for publishing nothing.
fn stood_down(report: BatchReport, reason: &str) -> BatchReport {
    BatchReport {
        stood_down: Some(reason.to_string()),
        ..report
    }
}

/// What one file's step leaves the run to do.
enum Next {
    Go,
    StandDown(StandDown),
    Stop(ActualError),
}

/// A run past its sign-in, with the repository's decisions read.
struct Publishing<'a> {
    run: Run,
    root: &'a Path,
    today: &'a str,
    progress: &'a mut dyn Write,
    cloud: Cloud,
    book: Book,
    report: BatchReport,
    /// Files sent so far, which `push`'s limit counts.
    sent: usize,
}

impl Publishing<'_> {
    /// Send a merged file's default branch copy and accept its draft.
    async fn accept_merged(&mut self, item: Merged) -> Next {
        if self.run.at_limit(self.sent) {
            self.report.left_for_next_run.push(item.name);
            return Next::Go;
        }
        if let Some((outcome, note)) = merged_settled(&self.cloud, &item) {
            let _ = writeln!(self.progress, "{}: skipped, {}", note.file, note.reason);
            let entry = Entry {
                outcome,
                at: Utc::now(),
                ..item.entry
            };
            self.book.note(&item.name, entry);
            self.report.skipped.push(note);
            return Next::Go;
        }
        self.sent += 1;
        let imported = match import(&self.cloud, &item.name, &item.file.body).await {
            Ok(imported) => imported,
            Err(e) => match unsent_by(e, self.run) {
                Ok(reason) => {
                    let _ = writeln!(self.progress, "{}: not accepted, {reason}", item.name);
                    let entry = Entry {
                        outcome: Outcome::Skipped,
                        at: Utc::now(),
                        ..item.entry
                    };
                    self.book.note(&item.name, entry);
                    self.report.not_saved.push(FileNote {
                        file: item.name,
                        reason,
                    });
                    return Next::Go;
                }
                Err(next) => return self.halt(&item.name, next),
            },
        };
        let outcome = finish_file_save(
            &self.cloud.session,
            &self.cloud.repo,
            self.root,
            &item.file,
            &item.doc,
            SaveStatus::Accepted,
            &imported.adr_id,
            Rename::Never,
        )
        .await;
        let accepted = outcome.created.status == SaveStatus::Accepted.as_str();
        let _ = writeln!(
            self.progress,
            "{}: on the default branch, {}",
            item.name,
            saved_text(&outcome)
        );
        let entry = Entry {
            outcome: if accepted {
                Outcome::Accepted
            } else {
                Outcome::WaitsForMerge
            },
            adr_id: Some(imported.adr_id.clone()),
            at: Utc::now(),
            ..item.entry
        };
        self.book.note(&item.name, entry);
        self.report.merged.push(Saved {
            file: item.name,
            already_saved: imported.already_saved,
            waits_for_merge: !accepted,
            outcome,
        });
        Next::Go
    }

    /// Send one file, or list why not.
    async fn send(&mut self, step: &str, item: &Pending<'_>) -> Next {
        if self
            .report
            .merged
            .iter()
            .any(|saved| saved.file == item.name)
        {
            // Its default branch copy went up this run.
            return Next::Go;
        }
        let own = self.book.own_id(&item.name);
        let (doc, status) = match plan(item, &self.cloud, own.as_deref(), self.run, self.today) {
            Ok(planned) => planned,
            Err(Unsent::Skip(reason)) => {
                self.skip(step, item, reason, Outcome::Skipped, own.as_deref());
                return Next::Go;
            }
            Err(Unsent::NotSaved(reason)) => {
                self.not_saved(step, item, reason, own.as_deref());
                return Next::Go;
            }
        };
        if self.run.at_limit(self.sent) {
            self.report.left_for_next_run.push(item.name.clone());
            return Next::Go;
        }
        self.sent += 1;
        let imported = match import(&self.cloud, &item.name, &item.file.body).await {
            Ok(imported) => imported,
            Err(e) => match unsent_by(e, self.run) {
                Ok(reason) => {
                    self.not_saved(step, item, reason, own.as_deref());
                    return Next::Go;
                }
                Err(next) => return self.halt(step, next),
            },
        };
        if let Some(found) = no_longer_draft(&self.cloud, &imported) {
            // The path's decision is no draft, so Actual AI owns it and the
            // file's text stays out of it.
            let (reason, outcome) = (already(found), owned(found));
            self.skip(step, item, reason, outcome, Some(&imported.adr_id));
            return Next::Go;
        }
        let (save_as, rename) = match status {
            SaveStatus::Accepted if self.run.interactive => {
                (SaveStatus::Accepted, Rename::IfAccepted)
            }
            _ => (SaveStatus::Draft, Rename::Never),
        };
        let outcome = finish_file_save(
            &self.cloud.session,
            &self.cloud.repo,
            self.root,
            item.file,
            &doc,
            save_as,
            &imported.adr_id,
            rename,
        )
        .await;
        let accepted = outcome.created.status == SaveStatus::Accepted.as_str();
        let waits = status == SaveStatus::Accepted && !accepted;
        let _ = writeln!(self.progress, "{step}: {}", saved_text(&outcome));
        let recorded = if accepted {
            Outcome::Accepted
        } else if waits {
            Outcome::WaitsForMerge
        } else {
            Outcome::Draft
        };
        self.book.note(
            &item.name,
            Entry::new(&item.text, recorded, Some(&imported.adr_id)),
        );
        self.cloud.comparable.push(ExistingDecision {
            id: imported.adr_id.clone(),
            title: doc.title.clone(),
            policies: statements(&doc),
        });
        self.report.saved.push(Saved {
            file: item.name.clone(),
            already_saved: imported.already_saved,
            waits_for_merge: waits,
            outcome,
        });
        Next::Go
    }

    /// `next`, with a progress line when it stops the run.
    fn halt(&mut self, step: &str, next: Next) -> Next {
        if let Next::Stop(e) = &next {
            let _ = writeln!(self.progress, "{step}: stopped, {e}");
        }
        next
    }

    fn skip(
        &mut self,
        step: &str,
        item: &Pending<'_>,
        reason: String,
        outcome: Outcome,
        adr_id: Option<&str>,
    ) {
        let _ = writeln!(self.progress, "{step}: skipped, {reason}");
        self.book
            .note(&item.name, Entry::new(&item.text, outcome, adr_id));
        self.report.skipped.push(FileNote {
            file: item.name.clone(),
            reason,
        });
    }

    fn not_saved(&mut self, step: &str, item: &Pending<'_>, reason: String, own: Option<&str>) {
        let _ = writeln!(self.progress, "{step}: not saved, {reason}");
        self.book
            .note(&item.name, Entry::new(&item.text, Outcome::Skipped, own));
        self.report.not_saved.push(FileNote {
            file: item.name.clone(),
            reason,
        });
    }

    /// End the run as `push` standing down, holding `rest` when the reason
    /// holds.
    fn stand_down(
        mut self,
        stand_down: StandDown,
        rest: &[Pending<'_>],
    ) -> (BatchReport, Option<ActualError>) {
        if stand_down.hold {
            self.book.hold(rest);
        }
        (stood_down(self.report, &stand_down.reason), None)
    }

    /// End the run at `file`, which `e` stopped, with `rest` never sent.
    fn stop(
        mut self,
        file: String,
        e: ActualError,
        rest: Vec<String>,
    ) -> (BatchReport, Option<ActualError>) {
        self.report.stopped = Some(FileNote {
            file,
            reason: e.to_string(),
        });
        self.report.not_sent = rest;
        (self.report, Some(e))
    }
}

/// What an import's error means for the file: the reason Actual AI refused
/// it, so the run goes on; or `push` standing down for a sign-in that is
/// gone or a role that can't save; or anything else stopping the run.
fn unsent_by(e: ActualError, run: Run) -> Result<String, Next> {
    match e {
        ActualError::AdrInvalid { reasons } => {
            Ok(format!("Actual AI refused it: {}", reasons.join("; ")))
        }
        e => match quiet(&e) {
            Some(stand_down) if run.is_push() => Err(Next::StandDown(stand_down)),
            _ => Err(Next::Stop(e)),
        },
    }
}

/// The errors `push` meets by publishing nothing and saying nothing: a
/// sign-in that is gone, and a repository Actual AI hasn't connected or a
/// role that can't save decisions, whose files wait out the hold.
fn quiet(e: &ActualError) -> Option<StandDown> {
    match e {
        ActualError::NotLoggedIn => Some(StandDown {
            reason: "Not signed in.".to_string(),
            hold: false,
        }),
        ActualError::AdrRepoRequired(reason) => Some(StandDown {
            reason: reason.clone(),
            hold: true,
        }),
        ActualError::AdrAccessDenied { .. } => Some(StandDown {
            reason: e.to_string(),
            hold: true,
        }),
        _ => None,
    }
}

/// Sign in, pick the repository and read its decisions. `push` stands down
/// where `create --all` reports why it can't publish.
async fn connect(
    target: &Target<'_>,
    root: &Path,
    run: Run,
) -> Result<Result<Cloud, StandDown>, ActualError> {
    match open_cloud(target, root).await {
        Ok(cloud) => Ok(Ok(cloud)),
        Err(e) => match quiet(&e) {
            Some(stand_down) if run.is_push() => Ok(Err(stand_down)),
            _ => Err(e),
        },
    }
}

async fn open_cloud(target: &Target<'_>, root: &Path) -> Result<Cloud, ActualError> {
    let session = session(target.api).await?;
    let repo = match pick_repo(&session, target.repo, root).await? {
        RepoPick::Chosen(repo) => repo,
        RepoPick::Unresolved(reason) => {
            return Err(ActualError::AdrRepoRequired(format!(
                "Couldn't tell which connected repository the decisions belong to: {reason}."
            )))
        }
    };
    let existing = decisions(&session, Some(&repo.id))
        .await
        .map_err(|e| match e {
            ActualError::NotLoggedIn | ActualError::AdrAccessDenied { .. } => e,
            other => ActualError::AdrNotSaved(format!(
                "Couldn't read the repository's decisions, so nothing was sent: {other}"
            )),
        })?;
    let comparable = existing
        .iter()
        .filter(|decision| decision.state != "archived")
        .map(|decision| ExistingDecision {
            id: decision.id.clone(),
            title: decision.title.clone(),
            policies: decision
                .policies
                .iter()
                .map(|p| p.statement.clone())
                .collect(),
        })
        .collect();
    Ok(Cloud {
        session,
        repo,
        existing,
        comparable,
    })
}

/// Save `markdown` as the file at `name`, keyed on its path.
async fn import(cloud: &Cloud, name: &str, markdown: &str) -> Result<ImportResponse, ActualError> {
    cloud
        .session
        .client
        .import_decision(&ImportRequest {
            organization_id: cloud.session.org_id.clone(),
            repo_unique_id: cloud.repo.id.clone(),
            markdown: markdown.to_string(),
            source_path: Some(name.to_string()),
        })
        .await
}

/// Why a file isn't sent when `found` is its decision in Actual AI.
fn already(found: &Decision) -> String {
    format!(
        "already decision {} in Actual AI ({})",
        sanitize_server_text(&found.id),
        sanitize_server_text(&found.state)
    )
}

/// What the record keeps for a file whose decision, `found`, is no draft:
/// accepted while it governs, else skipped.
fn owned(found: &Decision) -> Outcome {
    if matches!(found.state.as_str(), "active" | "updated") {
        Outcome::Accepted
    } else {
        Outcome::Skipped
    }
}

/// The decision an import returned for a path saved before, when the read
/// at the start shows it is no longer a draft.
fn no_longer_draft<'c>(cloud: &'c Cloud, imported: &ImportResponse) -> Option<&'c Decision> {
    if !imported.already_saved {
        return None;
    }
    cloud
        .existing
        .iter()
        .find(|decision| decision.id == imported.adr_id && decision.state != "draft")
}

/// A merged file whose draft the read at the start shows is a draft no
/// longer: what to record, and the note to list it with.
fn merged_settled(cloud: &Cloud, item: &Merged) -> Option<(Outcome, FileNote)> {
    let id = item.entry.adr_id.as_deref()?;
    let found = cloud.existing.iter().find(|decision| decision.id == id)?;
    if found.state == "draft" {
        return None;
    }
    let note = FileNote {
        file: item.name.clone(),
        reason: already(found),
    };
    Some((owned(found), note))
}

/// The drafts the record says wait for their accepted file to reach the
/// default branch, whose file is now there, accepted and passing the check.
async fn merged_copies(book: &Book, root: &Path, today: &str) -> Vec<Merged> {
    let waiting: Vec<(&String, &Entry)> = book
        .record
        .files
        .iter()
        .filter(|(_, entry)| entry.outcome == Outcome::WaitsForMerge)
        .collect();
    if waiting.is_empty() {
        return Vec::new();
    }
    let Some(branch) = default_branch(root).await else {
        return Vec::new();
    };
    let mut merged = Vec::new();
    for (name, entry) in waiting {
        // A copy past the size limit fails the check, as a file in the
        // checkout does.
        let Some(text) = git(root, &["show", &format!("{branch}:./{name}")]).await else {
            continue;
        };
        let file = files::decision_file_from_text(&root.join(name), &text);
        let checked = check_markdown(&file.body, false, today).result;
        let (Ok(SaveStatus::Accepted), Ok(doc)) = (own_status(&file), checked) else {
            continue;
        };
        merged.push(Merged {
            name: name.clone(),
            file,
            doc,
            entry: entry.clone(),
        });
    }
    merged
}

/// The default branch as the last fetch left it: the one `origin/HEAD`
/// names, else `origin/main` or `origin/master`.
async fn default_branch(root: &Path) -> Option<String> {
    let head = git(
        root,
        &[
            "symbolic-ref",
            "--quiet",
            "--short",
            "refs/remotes/origin/HEAD",
        ],
    )
    .await;
    if let Some(head) = head
        .as_deref()
        .map(str::trim)
        .filter(|head| !head.is_empty())
    {
        return Some(head.to_string());
    }
    for branch in ["origin/main", "origin/master"] {
        let found = git(
            root,
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("refs/remotes/{branch}"),
            ],
        )
        .await;
        if found.is_some() {
            return Some(branch.to_string());
        }
    }
    None
}

/// What `git <args>` prints in `root`, when it succeeds in time.
async fn git(root: &Path, args: &[&str]) -> Option<String> {
    let mut cmd = tokio::process::Command::new("git");
    cmd.args(args)
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let child = cmd.spawn().ok()?;
    match tokio::time::timeout(GIT_TIMEOUT, child.wait_with_output()).await {
        Ok(Ok(output)) if output.status.success() => String::from_utf8(output.stdout).ok(),
        _ => None,
    }
}

/// `path` under `root` as the repository names it, with `/` between its
/// parts: `docs/adr/use-postgres.md`.
fn source_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .components()
        .filter_map(|part| match part {
            Component::Normal(part) => Some(part.to_string_lossy()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// The decision id a file's name starts with, as the sync names a decision
/// doc: `<id>-<slug>.md`.
fn name_id(path: &Path) -> Option<String> {
    let stem = path.file_stem()?.to_str()?;
    let id = stem.get(..36)?;
    let rest = &stem[36..];
    if !rest.is_empty() && !rest.starts_with('-') {
        return None;
    }
    Uuid::parse_str(id).ok().map(|id| id.to_string())
}

/// The decision to send from `item` and the status its header names; or,
/// for a file Actual AI already has by an id in its header or name, one
/// that doesn't govern, one that fails the check, or a likely duplicate
/// without `allow_duplicate`, why not. The decision `own` names, the one the
/// file was saved as, is never its duplicate.
fn plan(
    item: &Pending<'_>,
    cloud: &Cloud,
    own: Option<&str>,
    run: Run,
    today: &str,
) -> Result<(ContextFile, SaveStatus), Unsent> {
    let file = item.file;
    let ids = [file.adr_id().map(str::to_string), name_id(&file.path)];
    if let Some(found) = ids.iter().flatten().find_map(|id| {
        cloud
            .existing
            .iter()
            .find(|decision| decision.id.eq_ignore_ascii_case(id))
    }) {
        return Err(Unsent::Skip(already(found)));
    }
    let status = own_status(file).map_err(Unsent::Skip)?;
    if let Err(reasons) = check_markdown(&file.body, file.is_draft(), today).result {
        return Err(Unsent::NotSaved(format!(
            "it doesn't pass `actual adr check`: {}",
            reasons.join("; ")
        )));
    }
    let doc = context_file::parse(&file.body)
        .map_err(Unsent::NotSaved)?
        .doc;
    let others: Vec<ExistingDecision> = cloud
        .comparable
        .iter()
        .filter(|decision| Some(decision.id.as_str()) != own)
        .cloned()
        .collect();
    let overlaps = find_overlaps(&doc.title, &statements(&doc), &others);
    if !run.allow_duplicate && !overlaps.is_empty() {
        let like: Vec<String> = overlaps
            .iter()
            .map(|overlap| {
                format!(
                    "{} \"{}\"",
                    sanitize_server_text(&overlap.adr_id),
                    sanitize_server_text(&overlap.title)
                )
            })
            .collect();
        return Err(Unsent::Skip(format!("looks like {}", like.join(", "))));
    }
    Ok((doc, status))
}

fn statements(doc: &ContextFile) -> Vec<String> {
    doc.policies.iter().map(|p| p.statement.clone()).collect()
}

/// What a file becomes, by its own status: a draft or proposed file a cloud
/// draft, and a file under a status that governs here an accepted decision.
/// Any other status keeps the file from being sent.
fn own_status(file: &DecisionFile) -> Result<SaveStatus, String> {
    let meta = file.meta.clone().unwrap_or_default();
    let proposed = meta
        .status
        .as_deref()
        .is_some_and(|status| status.trim().eq_ignore_ascii_case("proposed"));
    if file.is_draft() || proposed {
        Ok(SaveStatus::Draft)
    } else if meta.is_active() {
        Ok(SaveStatus::Accepted)
    } else {
        Err(format!(
            "its status is {}, under which it doesn't govern",
            sanitize_server_text(meta.status.as_deref().unwrap_or_default())
        ))
    }
}

/// One saved file's progress line.
fn saved_text(outcome: &CreateOutcome) -> String {
    let created = &outcome.created;
    let mut text = format!(
        "saved as {}, {}",
        if created.status == SaveStatus::Accepted.as_str() {
            "accepted"
        } else {
            "a draft"
        },
        sanitize_server_text(&created.adr_id)
    );
    if outcome.scope_problem.is_some() {
        text.push_str(" (its scope wasn't set)");
    } else if outcome.publish_problem.is_some() {
        text.push_str(" (publishing it failed)");
    }
    text
}

fn count(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// `create --all`'s summary.
fn render(report: &BatchReport) -> String {
    let mut out = String::new();
    if report.repo_unique_id.is_none() {
        let _ = writeln!(out, "No decision files under {DECISION_DIR}/ to send.");
    } else {
        let accepted = report
            .saved
            .iter()
            .filter(|saved| saved.outcome.created.status == SaveStatus::Accepted.as_str())
            .count();
        let _ = writeln!(
            out,
            "{} Saved {} of {} in {}: {} accepted, {}.",
            theme::success(&theme::SUCCESS),
            report.saved.len(),
            count(report.files, "decision file", "decision files"),
            sanitize_server_text(&report.repo_label),
            accepted,
            count(report.saved.len() - accepted, "draft", "drafts"),
        );
        let waiting = report
            .saved
            .iter()
            .filter(|saved| saved.waits_for_merge)
            .count();
        if waiting > 0 {
            let _ = writeln!(
                out,
                "{} marked accepted {} a draft until {} on the default branch, as no person \
                 is at the terminal.",
                count(waiting, "file", "files"),
                if waiting == 1 { "is" } else { "are" },
                if waiting == 1 { "it is" } else { "each is" },
            );
        }
        if accepted > 0 || report.merged.iter().any(|saved| !saved.waits_for_merge) {
            out.push_str(
                "Actual AI opens a pull request that writes the accepted decisions' rule files.\n",
            );
        }
    }
    out.push_str(&render_merged(report));
    out.push_str(&render_notes(report));
    for saved in &report.saved {
        let outcome = &saved.outcome;
        for problem in [&outcome.scope_problem, &outcome.publish_problem]
            .into_iter()
            .flatten()
        {
            let _ = writeln!(
                out,
                "{} {}: {problem}",
                theme::warning("warning:"),
                saved.file
            );
        }
        for problem in outcome.files.iter().flat_map(|files| &files.problems) {
            let _ = writeln!(out, "{} {}: {problem}", theme::warning("note:"), saved.file);
        }
    }
    out.push_str(&render_stopped(report));
    if !report.other_files.is_empty() {
        let _ = writeln!(
            out,
            "\n{} under {DECISION_DIR}/ {} no ADR header; `actual adr discover` lists every \
             ADR file and its layout.",
            count(report.other_files.len(), "markdown file", "markdown files"),
            if report.other_files.len() == 1 {
                "has"
            } else {
                "have"
            }
        );
    }
    out
}

/// `push`'s summary: nothing when it sent nothing or stood down.
fn render_push(report: &BatchReport) -> String {
    let quiet = report.saved.is_empty()
        && report.merged.is_empty()
        && report.skipped.is_empty()
        && report.not_saved.is_empty()
        && report.stopped.is_none();
    if report.stood_down.is_some() || quiet {
        return String::new();
    }
    let mut out = String::new();
    if !report.saved.is_empty() {
        let _ = writeln!(
            out,
            "{} Sent {} to Actual AI in {}:",
            theme::success(&theme::SUCCESS),
            count(report.saved.len(), "decision file", "decision files"),
            sanitize_server_text(&report.repo_label)
        );
    }
    for saved in &report.saved {
        let _ = write!(out, "  {}: {}", saved.file, saved_text(&saved.outcome));
        if saved.waits_for_merge {
            out.push_str(", accepted once it is on the default branch");
        }
        out.push('\n');
    }
    out.push_str(&render_merged(report));
    out.push_str(&render_notes(report));
    for saved in report.saved.iter().chain(&report.merged) {
        let outcome = &saved.outcome;
        for problem in [&outcome.scope_problem, &outcome.publish_problem]
            .into_iter()
            .flatten()
        {
            let _ = writeln!(
                out,
                "{} {}: {problem}",
                theme::warning("warning:"),
                saved.file
            );
        }
    }
    out.push_str(&render_stopped(report));
    if !report.left_for_next_run.is_empty() {
        let _ = writeln!(
            out,
            "{} the next run.",
            count(
                report.left_for_next_run.len(),
                "more file waits for",
                "more files wait for"
            )
        );
    }
    out
}

/// The drafts accepted this run now that their files are on the default
/// branch.
fn render_merged(report: &BatchReport) -> String {
    let mut out = String::new();
    if report.merged.is_empty() {
        return out;
    }
    let _ = writeln!(
        out,
        "\nOn the default branch now ({}):",
        report.merged.len()
    );
    for saved in &report.merged {
        let _ = writeln!(out, "  {}: {}", saved.file, saved_text(&saved.outcome));
    }
    out
}

fn render_notes(report: &BatchReport) -> String {
    let mut out = String::new();
    for (heading, notes) in [
        ("Skipped", &report.skipped),
        ("Not saved", &report.not_saved),
    ] {
        if !notes.is_empty() {
            let _ = writeln!(out, "\n{heading} ({}):", notes.len());
        }
        for note in notes {
            let _ = writeln!(out, "  {}: {}", note.file, note.reason);
        }
    }
    out
}

fn render_stopped(report: &BatchReport) -> String {
    let mut out = String::new();
    if let Some(stopped) = &report.stopped {
        let _ = writeln!(
            out,
            "\nStopped at {}, so {} after it {} sent.",
            stopped.file,
            count(report.not_sent.len(), "file", "files"),
            if report.not_sent.len() == 1 {
                "wasn't"
            } else {
                "weren't"
            }
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::super::cloud::Created;
    use super::super::tests::{api, git_repo, sign_in, CONNECTED, ORG, REPO};
    use super::*;
    use crate::adr::files::RenameOutcome;
    use crate::cli::args::{AdrArgs, AdrContentArgs, AdrNewArgs};
    use crate::testutil::{EnvGuard, ENV_MUTEX};
    use clap::Parser;
    use mockito::Matcher;
    use serde_json::json;
    use std::path::PathBuf;
    use tempfile::tempdir;

    const READ: &str = "/v1/gcp/product/decisions";
    const IMPORT: &str = "/v1/gcp/product/adr-import";
    const PUBLISH: &str = "/v1/gcp/product/adr-publish";
    const ORIGIN: &str = "https://github.com/actual-software/actual-cli";
    const TODAY: &str = "2026-11-18";
    const SYNCED: &str = "aaaaaaaa-0000-4000-8000-000000000001";
    const KAFKA: &str = "aaaaaaaa-0000-4000-8000-000000000002";
    const NEW_A: &str = "bbbbbbbb-0000-4000-8000-00000000000a";
    const NEW_B: &str = "bbbbbbbb-0000-4000-8000-00000000000b";
    const NEW_I: &str = "bbbbbbbb-0000-4000-8000-00000000000c";

    /// A decision as the list read returns it.
    fn web_json(id: &str, title: &str, status: &str, policy: &str) -> String {
        format!(
            r#"{{"id":"{id}","date":"2026-11-02T09:00:00.000Z","title":"{title}","context":"Why.",
                "status":"{status}","scope":{{"root":".","paths":[]}},"sourceRepoUniqueId":"{REPO}",
                "schemaVersion":2,"policyRules":[{{"id":"rule-1","adrId":"{id}","level":"MUST",
                "statement":"{policy}"}}],"candidateGroupId":"{id}","memberAdrIds":["{id}"],
                "kind":"single","primaryAdrId":"{id}"}}"#
        )
    }

    fn read_body(adrs: &[String]) -> String {
        format!(
            r#"{{"adrs":[{}],"suggestions":[],"version":1,"repository_summaries":[]}}"#,
            adrs.join(",")
        )
    }

    fn all_args(server: &mockito::Server) -> AdrCreateArgs {
        AdrCreateArgs {
            file: None,
            title: None,
            content: AdrContentArgs::default(),
            status: None,
            repo: None,
            allow_duplicate: false,
            all: true,
            api: api(server),
        }
    }

    /// `create --all` with a person at the terminal, or without one.
    fn create_all(interactive: bool, allow_duplicate: bool) -> Run {
        Run {
            interactive,
            limit: None,
            allow_duplicate,
        }
    }

    /// `push`, with its limit.
    fn push(limit: usize) -> Run {
        Run {
            interactive: false,
            limit: Some(limit),
            allow_duplicate: false,
        }
    }

    /// One publishing run over `root`, as `args` names the repository.
    fn publish(
        args: &AdrCreateArgs,
        root: &Path,
        run: Run,
        progress: &mut Vec<u8>,
    ) -> Result<(BatchReport, Option<ActualError>), ActualError> {
        let target = Target {
            repo: args.repo.as_deref(),
            api: &args.api,
        };
        block(run_publish(&target, root, run, TODAY, progress))
    }

    /// A pair `adr new` writes, its decision doc then moved to `docs/adr/<name>`
    /// so the run takes the files in a known order.
    fn pair(
        root: &Path,
        name: &str,
        title: &str,
        policy: &str,
        scope: &[&str],
        draft: bool,
    ) -> PathBuf {
        let args = AdrNewArgs {
            title: title.to_string(),
            content: AdrContentArgs {
                context: vec!["Why.".to_string()],
                policies: vec![policy.to_string()],
            },
            deciders: None,
            scope: scope.iter().map(|s| s.to_string()).collect(),
            status: Some(if draft {
                SaveStatus::Draft
            } else {
                SaveStatus::Accepted
            }),
            root: Some(root.to_path_buf()),
            json: false,
        };
        let written = super::super::local::write_new(&args, root, true, "2026-11-11").unwrap();
        let to = root.join(DECISION_DIR).join(name);
        std::fs::create_dir_all(to.parent().unwrap()).unwrap();
        std::fs::rename(&written.decision_file, &to).unwrap();
        to
    }

    /// A decision doc written by hand, with no rule file.
    fn doc(root: &Path, name: &str, id: &str, status: &str, body: &str) {
        let header = format!(
            "---\nversion: 2\nname: n\ndescription: d\nmetadata:\n  actual:\n    adr_id: {id}\n    \
             status: {status}\n  scope:\n    root: .\n---\n\n"
        );
        let path = root.join(DECISION_DIR).join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, header + body).unwrap();
    }

    const DECISION: &str =
        "# Use Redis\n\n## Context\n\nWhy.\n\n## Decision\n\n1. MUST: Cache reads.\n";

    fn markdown(path: &Path) -> String {
        files::read_decision_file(path).unwrap().body
    }

    /// The import of `path` under `root`, keyed on its path.
    fn import(
        server: &mut mockito::Server,
        root: &Path,
        path: &Path,
        status: usize,
        body: &str,
    ) -> mockito::Mock {
        // Each save carries the signed-in session's token, as `create --file`'s does.
        server
            .mock("POST", IMPORT)
            .match_header("authorization", "Bearer tok")
            .match_body(Matcher::PartialJson(json!({
                "markdown": markdown(path),
                "source_path": source_path(root, path),
            })))
            .with_status(status)
            .with_body(body)
            .expect(1)
            .create()
    }

    fn saved(id: &str, already: bool) -> String {
        format!(r#"{{"success":true,"adr_id":"{id}","already_saved":{already}}}"#)
    }

    fn connected(server: &mut mockito::Server) -> mockito::Mock {
        server
            .mock("GET", "/v1/connected-repos")
            .match_query(Matcher::Any)
            .with_status(200)
            .with_body(CONNECTED)
            .create()
    }

    fn read(server: &mut mockito::Server, adrs: &[String]) -> mockito::Mock {
        server
            .mock("GET", READ)
            .match_query(Matcher::Any)
            .with_status(200)
            .with_body(read_body(adrs))
            .create()
    }

    fn published(server: &mut mockito::Server, id: &str) -> mockito::Mock {
        server
            .mock("POST", PUBLISH)
            .match_body(Matcher::Json(json!({"organization_id": ORG, "adr_id": id})))
            .with_status(200)
            .with_body(r#"{"success":true}"#)
            .expect(1)
            .create()
    }

    /// A server whose mocks fail the test when a run sends it any request.
    fn quiet_server() -> (mockito::ServerGuard, [mockito::Mock; 2]) {
        let mut server = mockito::Server::new();
        let mocks = ["GET", "POST"].map(|method| {
            server
                .mock(method, Matcher::Any)
                .with_status(500)
                .expect(0)
                .create()
        });
        (server, mocks)
    }

    fn block<T>(future: impl std::future::Future<Output = T>) -> T {
        runtime().unwrap().block_on(future)
    }

    /// Run git in `dir`, as a committer that needs no configuration.
    fn git_in(dir: &Path, args: &[&str]) {
        let output = std::process::Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
            .args(["-c", "commit.gpgsign=false"])
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(output.status.success(), "git {args:?}: {output:?}");
    }

    /// Commit each file under `root` with the text given, point
    /// `origin/main` at the commit as a fetch would, and put the checkout's
    /// own text back.
    fn merge(root: &Path, files: &[(&str, &str)]) {
        let mut kept = Vec::new();
        for (name, text) in files {
            let path = root.join(name);
            kept.push((path.clone(), std::fs::read_to_string(&path).ok()));
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, text).unwrap();
            git_in(root, &["add", name]);
        }
        git_in(root, &["commit", "-q", "--allow-empty", "-m", "merge"]);
        git_in(root, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
        for (path, text) in kept {
            match text {
                Some(text) => std::fs::write(path, text).unwrap(),
                None => std::fs::remove_file(path).unwrap(),
            }
        }
    }

    /// The record the runs keep for the test checkout, by file.
    fn recorded(config: &Path) -> std::collections::BTreeMap<String, Entry> {
        let key = Key {
            org_id: ORG.to_string(),
            repo: "actual-software/actual-cli".to_string(),
        };
        record::load(&config.join("adr-push"), &key).files
    }

    #[test]
    fn create_all_sends_new_files_with_their_own_status_and_lists_the_rest() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempdir().unwrap();
        let _guards = sign_in(tmp.path());
        let repo = tempdir().unwrap();
        let root = repo.path();
        git_repo(root, Some(ORIGIN));
        let a = pair(
            root,
            "a-postgres.md",
            "Use Postgres for storage",
            "MUST: Store relational data in Postgres.",
            &["src/**"],
            false,
        );
        let b = pair(
            root,
            "b-sessions.md",
            "Cache sessions in memory",
            "SHOULD: Keep session state in process memory.",
            &[],
            true,
        );
        doc(root, "c-synced.md", SYNCED, "accepted", DECISION);
        doc(root, "d-retired.md", "local-d", "superseded", DECISION);
        doc(
            root,
            "e-broken.md",
            "local-e",
            "accepted",
            "# Broken\n\nNo decision here.\n",
        );
        pair(
            root,
            "f-kafka.md",
            "Use Kafka for events",
            "MUST: Publish domain events to Kafka.",
            &[],
            false,
        );
        pair(
            root,
            "g-postgres-again.md",
            "Use Postgres for storage",
            "MUST: Keep backups for thirty days.",
            &[],
            false,
        );
        let h = pair(
            root,
            "h-graphql.md",
            "Adopt GraphQL",
            "SHOULD: Serve the web API with GraphQL.",
            &[],
            true,
        );
        let i = pair(
            root,
            "i-json-logs.md",
            "Write structured logs",
            "MUST: Write log lines as JSON objects.",
            &[],
            true,
        );
        std::fs::write(root.join("docs/adr/README.md"), "# Decisions\n").unwrap();
        // Nothing under docs/adr/drafts/ is sent or listed.
        pair(
            root,
            "drafts/later.md",
            "Use Rust everywhere",
            "MUST: Write services in Rust.",
            &[],
            true,
        );
        std::fs::write(root.join("docs/adr/drafts/notes.md"), "# Notes\n").unwrap();

        let mut server = mockito::Server::new();
        connected(&mut server);
        let cloud = [
            web_json(SYNCED, "Use Redis for caching", "accepted", "Cache reads."),
            web_json(KAFKA, "Use Kafka for events", "accepted", "Publish events."),
        ];
        let first_read = server
            .mock("GET", READ)
            .match_query(Matcher::Any)
            .with_status(200)
            .with_body(read_body(&cloud))
            .expect(1)
            .create();
        let saved_a = web_json(
            NEW_A,
            "Use Postgres for storage",
            "draft",
            "Store relational data in Postgres.",
        );
        let scope_read = server
            .mock("GET", READ)
            .match_query(Matcher::Any)
            .with_status(200)
            .with_body(read_body(&[cloud[0].clone(), cloud[1].clone(), saved_a]))
            .expect(1)
            .create();
        let imports = [
            import(&mut server, root, &a, 200, &saved(NEW_A, false)),
            import(&mut server, root, &b, 200, &saved(NEW_B, false)),
            import(
                &mut server,
                root,
                &h,
                400,
                r#"{"message":"The title is too short."}"#,
            ),
            import(&mut server, root, &i, 200, &saved(NEW_I, false)),
        ];
        let scope = server
            .mock("POST", "/v1/gcp/product/adr-group-update")
            .match_body(Matcher::PartialJson(json!({
                "group_id": NEW_A,
                "content": {"scope": {"root": ".", "paths": ["src/**"]}}
            })))
            .with_status(200)
            .with_body(format!(
                r#"{{"success":true,"members":[{{"adr_id":"{NEW_A}"}}]}}"#
            ))
            .expect(1)
            .create();
        let publish_a = published(&mut server, NEW_A);

        let mut progress = Vec::new();
        let (report, stop) = publish(
            &all_args(&server),
            root,
            create_all(true, false),
            &mut progress,
        )
        .unwrap();

        first_read.assert();
        scope_read.assert();
        for mock in &imports {
            mock.assert();
        }
        scope.assert();
        publish_a.assert();
        assert!(stop.is_none());
        let progress = String::from_utf8(progress).unwrap();
        let lines: Vec<&str> = progress.lines().collect();
        assert_eq!(lines.len(), 9, "{progress}");
        assert_eq!(
            lines[0],
            format!("[1/9] docs/adr/a-postgres.md: saved as accepted, {NEW_A}")
        );
        assert_eq!(
            lines[1],
            format!("[2/9] docs/adr/b-sessions.md: saved as a draft, {NEW_B}")
        );
        assert_eq!(
            lines[2],
            format!("[3/9] docs/adr/c-synced.md: skipped, already decision {SYNCED} in Actual AI (active)")
        );
        assert_eq!(
            lines[3],
            "[4/9] docs/adr/d-retired.md: skipped, its status is superseded, under which it doesn't govern"
        );
        assert!(
            lines[4].starts_with(
                "[5/9] docs/adr/e-broken.md: not saved, it doesn't pass `actual adr check`: No policies found."
            ),
            "{progress}"
        );
        assert_eq!(
            lines[5],
            format!(
                "[6/9] docs/adr/f-kafka.md: skipped, looks like {KAFKA} \"Use Kafka for events\""
            )
        );
        assert_eq!(
            lines[6],
            format!("[7/9] docs/adr/g-postgres-again.md: skipped, looks like {NEW_A} \"Use Postgres for storage\"")
        );
        assert_eq!(
            lines[7],
            "[8/9] docs/adr/h-graphql.md: not saved, Actual AI refused it: The title is too short."
        );
        assert_eq!(
            lines[8],
            format!("[9/9] docs/adr/i-json-logs.md: saved as a draft, {NEW_I}")
        );

        assert_eq!(report.files, 9);
        assert_eq!(report.saved.len(), 3);
        assert_eq!((report.skipped.len(), report.not_saved.len()), (4, 2));
        assert_eq!(report.other_files, ["docs/adr/README.md"]);
        // The accepted pair took its new id at once; the drafts keep their
        // names, which the server ties to their drafts.
        assert!(!a.exists());
        assert!(files::find_rule_file(root, NEW_A).is_some());
        assert!(b.exists() && i.exists());
        assert!(files::read_decision_file(&b).unwrap().is_draft());
        assert!(root.join("docs/adr/drafts/later.md").exists());
        let record = recorded(tmp.path());
        assert_eq!(record["docs/adr/a-postgres.md"].outcome, Outcome::Accepted);
        assert_eq!(record["docs/adr/b-sessions.md"].outcome, Outcome::Draft);
        assert_eq!(
            record["docs/adr/b-sessions.md"].adr_id.as_deref(),
            Some(NEW_B)
        );
        assert_eq!(record["docs/adr/c-synced.md"].outcome, Outcome::Skipped);
        assert_eq!(record["docs/adr/h-graphql.md"].outcome, Outcome::Skipped);
        assert!(!record.contains_key("docs/adr/drafts/later.md"));

        let text = render(&report);
        assert!(
            text.contains(
                "Saved 3 of 9 decision files in actual-software/actual-cli: 1 accepted, 2 drafts."
            ),
            "{text}"
        );
        assert!(text.contains("opens a pull request"), "{text}");
        assert!(!text.contains("until"), "{text}");
        assert!(
            text.contains("\nSkipped (4):\n  docs/adr/c-synced.md: already decision"),
            "{text}"
        );
        assert!(
            text.contains("\nNot saved (2):\n  docs/adr/e-broken.md: "),
            "{text}"
        );
        assert!(
            text.contains("1 markdown file under docs/adr/ has no ADR header"),
            "{text}"
        );
        assert!(!text.contains("note:"), "{text}");
        let finished = finish(&report, stop).err();
        assert!(
            matches!(&finished, Some(ActualError::AdrCheckFailed(m)) if m == "2 of 9 decision files weren't saved"),
            "{finished:?}"
        );
        let json: serde_json::Value = serde_json::from_str(&json_text(&report)).unwrap();
        assert_eq!(json["saved"][0]["file"], "docs/adr/a-postgres.md");
        assert_eq!(json["saved"][0]["adr_id"], NEW_A);
        assert_eq!(json["saved"][0]["status"], "accepted");
        assert!(json["saved"][0].get("already_saved").is_none());
        assert_eq!(json["repo_unique_id"], REPO);
        assert!(json.get("stopped").is_none());
        assert!(json.get("stood_down").is_none());
    }

    #[test]
    fn create_all_with_no_person_at_the_terminal_leaves_accepted_files_as_drafts() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempdir().unwrap();
        let _guards = sign_in(tmp.path());
        let repo = tempdir().unwrap();
        let root = repo.path();
        git_repo(root, Some(ORIGIN));
        let a = pair(
            root,
            "a.md",
            "Use Postgres for storage",
            "MUST: Store data in Postgres.",
            &[],
            false,
        );
        doc(root, "b-proposed.md", "local-b", "proposed", DECISION);
        let mut server = mockito::Server::new();
        connected(&mut server);
        read(&mut server, &[]);
        let sent = [
            import(&mut server, root, &a, 200, &saved(NEW_A, false)),
            import(
                &mut server,
                root,
                &root.join("docs/adr/b-proposed.md"),
                200,
                &saved(NEW_B, false),
            ),
        ];
        let never_published = server.mock("POST", PUBLISH).expect(0).create();
        let (report, stop) = publish(
            &all_args(&server),
            root,
            create_all(false, false),
            &mut Vec::new(),
        )
        .unwrap();
        for mock in &sent {
            mock.assert();
        }
        never_published.assert();
        assert!(stop.is_none());
        assert_eq!(report.saved.len(), 2);
        assert!(report.saved[0].waits_for_merge);
        assert_eq!(report.saved[0].outcome.created.status, "draft");
        // A proposed file is a draft, and waits for nothing.
        assert!(!report.saved[1].waits_for_merge);
        assert!(a.exists(), "a draft keeps its file's name");
        let record = recorded(tmp.path());
        assert_eq!(record["docs/adr/a.md"].outcome, Outcome::WaitsForMerge);
        assert_eq!(record["docs/adr/b-proposed.md"].outcome, Outcome::Draft);
        let text = render(&report);
        assert!(
            text.contains("0 accepted, 2 drafts.\n1 file marked accepted is a draft until it is on the default branch"),
            "{text}"
        );
        assert!(!text.contains("pull request"), "{text}");
        let json: serde_json::Value = serde_json::from_str(&json_text(&report)).unwrap();
        assert_eq!(json["saved"][0]["waits_for_merge"], true);
    }

    #[test]
    fn create_all_stops_at_an_unknown_outcome_and_names_what_it_never_sent() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempdir().unwrap();
        let _guards = sign_in(tmp.path());
        let repo = tempdir().unwrap();
        let root = repo.path();
        let a = pair(
            root,
            "a.md",
            "Use Postgres for storage",
            "MUST: Store data in Postgres.",
            &[],
            true,
        );
        pair(
            root,
            "b.md",
            "Use Kafka for events",
            "MUST: Publish events to Kafka.",
            &[],
            true,
        );
        pair(
            root,
            "c.md",
            "Write structured logs",
            "MUST: Log JSON.",
            &[],
            true,
        );
        let mut server = mockito::Server::new();
        read(&mut server, &[]);
        let sent = import(&mut server, root, &a, 200, "not json");
        let args = AdrCreateArgs {
            repo: Some(REPO.to_string()),
            ..all_args(&server)
        };
        let mut progress = Vec::new();
        let (report, stop) = publish(&args, root, create_all(true, false), &mut progress).unwrap();
        sent.assert();
        assert!(
            matches!(&stop, Some(ActualError::AdrCreateUnconfirmed { .. })),
            "{stop:?}"
        );
        assert_eq!(
            report.stopped.as_ref().map(|s| s.file.as_str()),
            Some("docs/adr/a.md")
        );
        assert_eq!(report.not_sent, ["docs/adr/b.md", "docs/adr/c.md"]);
        assert!(a.exists(), "a file whose save is unknown keeps its name");
        let progress = String::from_utf8(progress).unwrap();
        assert!(
            progress.starts_with("[1/3] docs/adr/a.md: stopped, "),
            "{progress}"
        );
        let text = render(&report);
        assert!(text.contains("Saved 0 of 3 decision files in"), "{text}");
        assert!(
            text.contains("Stopped at docs/adr/a.md, so 2 files after it weren't sent."),
            "{text}"
        );
        let one_left = BatchReport {
            not_sent: vec!["docs/adr/c.md".to_string()],
            ..report
        };
        assert!(render(&one_left).contains("so 1 file after it wasn't sent."));
        let finished = finish(&one_left, stop).err();
        assert!(matches!(
            finished,
            Some(ActualError::AdrCreateUnconfirmed { .. })
        ));
    }

    #[test]
    fn create_all_sends_a_look_alike_when_told_to() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempdir().unwrap();
        let _guards = sign_in(tmp.path());
        let repo = tempdir().unwrap();
        let root = repo.path();
        let a = pair(
            root,
            "a.md",
            "Use Kafka for events",
            "MUST: Publish events to Kafka.",
            &[],
            true,
        );
        let mut server = mockito::Server::new();
        let archived = read(
            &mut server,
            &[web_json(
                KAFKA,
                "Use Kafka for events",
                "archived-draft",
                "x",
            )],
        );
        let sent = import(&mut server, root, &a, 200, &saved(NEW_A, false));
        let args = AdrCreateArgs {
            repo: Some(REPO.to_string()),
            ..all_args(&server)
        };
        // An archived decision is no duplicate.
        let (report, _) = publish(&args, root, create_all(true, false), &mut Vec::new()).unwrap();
        assert_eq!(report.saved.len(), 1);
        sent.assert();
        archived.remove();

        let b = pair(
            root,
            "b.md",
            "Use Kafka for events",
            "MUST: Publish events to Kafka.",
            &[],
            true,
        );
        read(
            &mut server,
            &[
                web_json(KAFKA, "Use Kafka for events", "draft", "x"),
                web_json(
                    NEW_A,
                    "Use Kafka for events",
                    "draft",
                    "Publish events to Kafka.",
                ),
            ],
        );
        // a.md is the draft NEW_A was saved from, so that draft is no duplicate
        // of it; KAFKA is, until --allow-duplicate.
        let (report, _) = publish(&args, root, create_all(true, false), &mut Vec::new()).unwrap();
        let reasons: Vec<&str> = report.skipped.iter().map(|n| n.reason.as_str()).collect();
        assert_eq!(
            reasons,
            [
                format!("looks like {KAFKA} \"Use Kafka for events\""),
                format!(
                    "looks like {KAFKA} \"Use Kafka for events\", {NEW_A} \"Use Kafka for events\""
                )
            ],
            "{report:?}"
        );
        let again = [
            import(&mut server, root, &a, 200, &saved(NEW_A, true)),
            import(&mut server, root, &b, 200, &saved(NEW_B, false)),
        ];
        let args = AdrCreateArgs {
            allow_duplicate: true,
            ..args
        };
        let (report, _) = publish(&args, root, create_all(true, true), &mut Vec::new()).unwrap();
        for mock in &again {
            mock.assert();
        }
        assert_eq!(report.saved.len(), 2, "{report:?}");
        assert!(report.saved[0].already_saved);
        assert!(!report.saved[1].already_saved);
    }

    #[test]
    fn create_all_reads_its_files_before_signing_in() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::remove("ACTUAL_CONFIG");
        let tmp = tempdir().unwrap();
        let _g2 = EnvGuard::set("ACTUAL_CONFIG_DIR", tmp.path().to_str().unwrap());
        let server = mockito::Server::new();
        let repo = tempdir().unwrap();
        let root = repo.path();
        std::fs::create_dir_all(root.join(DECISION_DIR)).unwrap();
        std::fs::write(root.join("docs/adr/0001-old.md"), "# Old\n").unwrap();
        std::fs::write(
            root.join("docs/adr/big.md"),
            "x".repeat(files::MAX_FILE_SIZE as usize + 1),
        )
        .unwrap();
        let (report, stop) = publish(
            &all_args(&server),
            root,
            create_all(true, false),
            &mut Vec::new(),
        )
        .unwrap();
        assert!(stop.is_none());
        assert_eq!((report.files, report.not_saved.len()), (1, 1));
        let text = render(&report);
        assert!(
            text.starts_with("No decision files under docs/adr/ to send.\n"),
            "{text}"
        );
        assert!(text.contains("\n  docs/adr/big.md: "), "{text}");
        assert!(
            text.contains("1 markdown file under docs/adr/ has no ADR header"),
            "{text}"
        );
        assert!(matches!(
            finish(&report, None),
            Err(ActualError::AdrCheckFailed(_))
        ));

        // With a file to send, signing in comes next.
        pair(
            root,
            "a.md",
            "Use Postgres",
            "MUST: Use Postgres.",
            &[],
            true,
        );
        let err = publish(
            &all_args(&server),
            root,
            create_all(true, false),
            &mut Vec::new(),
        )
        .err();
        assert!(matches!(err, Some(ActualError::NotLoggedIn)), "{err:?}");
    }

    #[test]
    fn create_all_needs_a_repository_and_its_decisions_first() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempdir().unwrap();
        let _guards = sign_in(tmp.path());
        let repo = tempdir().unwrap();
        let root = repo.path();
        pair(
            root,
            "a.md",
            "Use Postgres",
            "MUST: Use Postgres.",
            &[],
            true,
        );
        let mut server = mockito::Server::new();
        let err = publish(
            &all_args(&server),
            root,
            create_all(true, false),
            &mut Vec::new(),
        )
        .err();
        assert!(
            matches!(&err, Some(ActualError::AdrRepoRequired(m)) if m.contains("no origin remote")),
            "{err:?}"
        );
        let args = AdrCreateArgs {
            repo: Some(REPO.to_string()),
            ..all_args(&server)
        };
        let failing = server
            .mock("GET", READ)
            .match_query(Matcher::Any)
            .with_status(500)
            .create();
        let err = publish(&args, root, create_all(true, false), &mut Vec::new()).err();
        assert!(
            matches!(&err, Some(ActualError::AdrNotSaved(m)) if m.starts_with("Couldn't read the repository's decisions")),
            "{err:?}"
        );
        failing.remove();
        server
            .mock("GET", READ)
            .match_query(Matcher::Any)
            .with_status(401)
            .create();
        let err = publish(&args, root, create_all(true, false), &mut Vec::new()).err();
        assert!(matches!(err, Some(ActualError::NotLoggedIn)), "{err:?}");
    }

    #[test]
    fn push_sends_new_files_as_drafts_then_nothing_until_one_changes() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempdir().unwrap();
        let _guards = sign_in(tmp.path());
        let repo = tempdir().unwrap();
        let root = repo.path();
        git_repo(root, Some(ORIGIN));
        let a = pair(
            root,
            "a.md",
            "Use Postgres for storage",
            "MUST: Store data in Postgres.",
            &[],
            false,
        );
        let b = pair(
            root,
            "b.md",
            "Cache sessions in memory",
            "SHOULD: Keep sessions in memory.",
            &[],
            true,
        );
        doc(root, "c-synced.md", SYNCED, "accepted", DECISION);
        let mut server = mockito::Server::new();
        connected(&mut server);
        let first_read = read(
            &mut server,
            &[web_json(SYNCED, "Use Redis", "accepted", "Cache reads.")],
        );
        let sent = [
            import(&mut server, root, &a, 200, &saved(NEW_A, false)),
            import(&mut server, root, &b, 200, &saved(NEW_B, false)),
        ];
        let never_published = server.mock("POST", PUBLISH).expect(0).create();
        let (report, stop) = publish(&all_args(&server), root, push(10), &mut Vec::new()).unwrap();
        for mock in &sent {
            mock.assert();
        }
        never_published.assert();
        assert!(stop.is_none());
        assert!(report.saved[0].waits_for_merge);
        assert!(a.exists() && b.exists(), "drafts keep their names");
        let text = render_push(&report);
        assert!(
            text.contains(&format!(
                "Sent 2 decision files to Actual AI in actual-software/actual-cli:\n  docs/adr/a.md: saved as a draft, {NEW_A}, accepted once it is on the default branch\n  docs/adr/b.md: saved as a draft, {NEW_B}\n"
            )),
            "{text}"
        );
        assert!(
            text.contains(&format!(
                "Skipped (1):\n  docs/adr/c-synced.md: already decision {SYNCED} in Actual AI (active)"
            )),
            "{text}"
        );
        first_read.remove();

        // Nothing changed, so the next run sends no request at all.
        let (quiet, mocks) = quiet_server();
        let (report, stop) = publish(&all_args(&quiet), root, push(10), &mut Vec::new()).unwrap();
        assert!(stop.is_none());
        assert!(report.saved.is_empty() && report.skipped.is_empty());
        assert_eq!(render_push(&report), "");
        for mock in &mocks {
            mock.assert();
        }

        // A changed draft goes up again, and the draft it was saved as is no
        // duplicate of it.
        let edited = std::fs::read_to_string(&b).unwrap().replace(
            "Keep sessions in memory.",
            "Keep sessions in process memory.",
        );
        std::fs::write(&b, edited).unwrap();
        read(
            &mut server,
            &[web_json(
                NEW_B,
                "Cache sessions in memory",
                "draft",
                "Keep sessions in memory.",
            )],
        );
        let resent = import(&mut server, root, &b, 200, &saved(NEW_B, true));
        let (report, _) = publish(&all_args(&server), root, push(10), &mut Vec::new()).unwrap();
        resent.assert();
        assert_eq!(report.saved.len(), 1, "{report:?}");
        assert!(report.saved[0].already_saved);
    }

    #[test]
    fn push_accepts_a_waiting_draft_with_the_default_branch_copy() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempdir().unwrap();
        let _guards = sign_in(tmp.path());
        let repo = tempdir().unwrap();
        let root = repo.path();
        git_repo(root, Some(ORIGIN));
        let a = pair(
            root,
            "a.md",
            "Use Postgres for storage",
            "MUST: Store data in Postgres.",
            &[],
            false,
        );
        let mut server = mockito::Server::new();
        connected(&mut server);
        let first_read = read(&mut server, &[]);
        let first = import(&mut server, root, &a, 200, &saved(NEW_A, false));
        publish(&all_args(&server), root, push(10), &mut Vec::new()).unwrap();
        first.assert();
        first_read.remove();
        assert_eq!(
            recorded(tmp.path())["docs/adr/a.md"].outcome,
            Outcome::WaitsForMerge
        );

        // Not on the default branch yet: nothing to send.
        let (quiet, mocks) = quiet_server();
        let (report, _) = publish(&all_args(&quiet), root, push(10), &mut Vec::new()).unwrap();
        assert!(report.merged.is_empty());
        for mock in &mocks {
            mock.assert();
        }

        // The review changed the policy before the merge; the local file
        // still has the old text.
        let local = std::fs::read_to_string(&a).unwrap();
        let reviewed = local.replace("Store data in Postgres.", "Store all data in Postgres.");
        std::fs::write(&a, &reviewed).unwrap();
        git_in(root, &["add", "docs/adr/a.md"]);
        git_in(root, &["commit", "-q", "-m", "decide"]);
        git_in(root, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
        std::fs::write(&a, &local).unwrap();

        read(
            &mut server,
            &[web_json(
                NEW_A,
                "Use Postgres for storage",
                "draft",
                "Store data in Postgres.",
            )],
        );
        let merged_text = files::decision_file_from_text(&a, &reviewed).body;
        let resent = server
            .mock("POST", IMPORT)
            .match_body(Matcher::PartialJson(json!({
                "markdown": merged_text,
                "source_path": "docs/adr/a.md",
            })))
            .with_status(200)
            .with_body(saved(NEW_A, true))
            .expect(1)
            .create();
        let accept = published(&mut server, NEW_A);
        let mut progress = Vec::new();
        let (report, stop) = publish(&all_args(&server), root, push(10), &mut progress).unwrap();
        resent.assert();
        accept.assert();
        assert!(stop.is_none());
        assert_eq!(report.merged.len(), 1, "{report:?}");
        assert_eq!(report.merged[0].outcome.created.status, "accepted");
        assert!(report.saved.is_empty(), "{report:?}");
        assert!(a.exists(), "the sync replaces the file, not the run");
        assert_eq!(
            String::from_utf8(progress).unwrap(),
            format!("docs/adr/a.md: on the default branch, saved as accepted, {NEW_A}\n")
        );
        assert_eq!(
            recorded(tmp.path())["docs/adr/a.md"].outcome,
            Outcome::Accepted
        );
        let text = render_push(&report);
        assert!(
            text.contains(&format!(
                "On the default branch now (1):\n  docs/adr/a.md: saved as accepted, {NEW_A}"
            )),
            "{text}"
        );
    }

    #[test]
    fn push_takes_a_draft_accepted_elsewhere_as_settled() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempdir().unwrap();
        let _guards = sign_in(tmp.path());
        let repo = tempdir().unwrap();
        let root = repo.path();
        git_repo(root, Some(ORIGIN));
        let a = pair(
            root,
            "a.md",
            "Use Postgres",
            "MUST: Use Postgres.",
            &[],
            false,
        );
        let mut server = mockito::Server::new();
        connected(&mut server);
        let first_read = read(&mut server, &[]);
        import(&mut server, root, &a, 200, &saved(NEW_A, false));
        publish(&all_args(&server), root, push(10), &mut Vec::new()).unwrap();
        first_read.remove();
        git_in(root, &["add", "docs/adr/a.md"]);
        git_in(root, &["commit", "-q", "-m", "decide"]);
        git_in(root, &["update-ref", "refs/remotes/origin/main", "HEAD"]);

        // Someone accepted it in the web app first, so nothing is sent.
        read(
            &mut server,
            &[web_json(NEW_A, "Use Postgres", "accepted", "Use Postgres.")],
        );
        let never = server.mock("POST", Matcher::Any).expect(0).create();
        let (report, _) = publish(&all_args(&server), root, push(10), &mut Vec::new()).unwrap();
        never.assert();
        assert_eq!(
            report.skipped[0].reason,
            format!("already decision {NEW_A} in Actual AI (active)")
        );
        assert_eq!(
            recorded(tmp.path())["docs/adr/a.md"].outcome,
            Outcome::Accepted
        );
    }

    #[test]
    fn push_leaves_a_path_saved_as_an_accepted_decision_alone() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempdir().unwrap();
        let _guards = sign_in(tmp.path());
        let repo = tempdir().unwrap();
        let root = repo.path();
        git_repo(root, Some(ORIGIN));
        let a = pair(
            root,
            "a.md",
            "Use Postgres",
            "MUST: Use Postgres.",
            &[],
            true,
        );
        let b = pair(
            root,
            "b.md",
            "Write structured logs",
            "MUST: Log JSON.",
            &[],
            true,
        );
        let mut server = mockito::Server::new();
        connected(&mut server);
        // A teammate's save of a.md was accepted and rewritten since, and
        // b.md's was archived.
        read(
            &mut server,
            &[
                web_json(NEW_A, "Keep events in Kafka", "accepted", "x"),
                web_json(NEW_B, "Retired", "archived-draft", "y"),
            ],
        );
        let sent = [
            import(&mut server, root, &a, 200, &saved(NEW_A, true)),
            import(&mut server, root, &b, 200, &saved(NEW_B, true)),
        ];
        let never = server.mock("POST", PUBLISH).expect(0).create();
        let (report, _) = publish(&all_args(&server), root, push(10), &mut Vec::new()).unwrap();
        for mock in &sent {
            mock.assert();
        }
        never.assert();
        assert!(report.saved.is_empty());
        assert_eq!(
            report.skipped[0].reason,
            format!("already decision {NEW_A} in Actual AI (active)")
        );
        assert_eq!(
            report.skipped[1].reason,
            format!("already decision {NEW_B} in Actual AI (archived)")
        );
        let record = recorded(tmp.path());
        assert_eq!(record["docs/adr/a.md"].adr_id.as_deref(), Some(NEW_A));
        assert_eq!(record["docs/adr/a.md"].outcome, Outcome::Accepted);
        assert_eq!(record["docs/adr/b.md"].outcome, Outcome::Skipped);
    }

    #[test]
    fn push_skips_a_file_named_for_a_decision_and_keeps_to_its_limit() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempdir().unwrap();
        let _guards = sign_in(tmp.path());
        let repo = tempdir().unwrap();
        let root = repo.path();
        git_repo(root, Some(ORIGIN));
        doc(
            root,
            &format!("{SYNCED}-use-redis.md"),
            "local-x",
            "accepted",
            DECISION,
        );
        let files: Vec<PathBuf> = [
            (
                "a",
                "Use Postgres for storage",
                "MUST: Store data in Postgres.",
            ),
            (
                "b",
                "Publish events to Kafka",
                "MUST: Send domain events to Kafka.",
            ),
            (
                "c",
                "Write structured logs",
                "SHOULD: Log lines as JSON objects.",
            ),
        ]
        .iter()
        .map(|(name, title, policy)| pair(root, &format!("{name}.md"), title, policy, &[], true))
        .collect();
        let mut server = mockito::Server::new();
        connected(&mut server);
        let first_read = read(
            &mut server,
            &[web_json(SYNCED, "Cache with Redis", "draft", "x")],
        );
        let first = [
            import(&mut server, root, &files[0], 200, &saved(NEW_A, false)),
            import(&mut server, root, &files[1], 200, &saved(NEW_B, false)),
        ];
        let (report, _) = publish(&all_args(&server), root, push(2), &mut Vec::new()).unwrap();
        for mock in &first {
            mock.assert();
        }
        assert_eq!(report.saved.len(), 2);
        assert_eq!(report.left_for_next_run, ["docs/adr/c.md"]);
        assert_eq!(
            report.skipped[0].reason,
            format!("already decision {SYNCED} in Actual AI (draft)")
        );
        assert!(render_push(&report).ends_with("1 more file waits for the next run.\n"));
        first_read.remove();

        read(&mut server, &[]);
        let rest = import(&mut server, root, &files[2], 200, &saved(NEW_I, false));
        let (report, _) = publish(&all_args(&server), root, push(2), &mut Vec::new()).unwrap();
        rest.assert();
        assert_eq!(report.saved.len(), 1);
        assert!(report.left_for_next_run.is_empty());
    }

    #[test]
    fn push_stands_down_quietly_and_holds_what_it_could_not_send() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempdir().unwrap();
        let repo = tempdir().unwrap();
        let root = repo.path();
        git_repo(root, Some("https://github.com/someone/elsewhere"));
        let a = pair(
            root,
            "a.md",
            "Use Postgres",
            "MUST: Use Postgres.",
            &[],
            true,
        );

        // Signed out: no request, no output.
        {
            let _g1 = EnvGuard::remove("ACTUAL_CONFIG");
            let _g2 = EnvGuard::set("ACTUAL_CONFIG_DIR", tmp.path().to_str().unwrap());
            let (quiet, mocks) = quiet_server();
            let (report, stop) =
                publish(&all_args(&quiet), root, push(10), &mut Vec::new()).unwrap();
            assert!(stop.is_none());
            assert_eq!(report.stood_down.as_deref(), Some("Not signed in."));
            assert_eq!(render_push(&report), "");
            for mock in &mocks {
                mock.assert();
            }
        }
        let mut server = mockito::Server::new();

        // A repository the organization hasn't connected: held for a day.
        let _guards = sign_in(tmp.path());
        let repos = connected(&mut server);
        let (report, stop) = publish(&all_args(&server), root, push(10), &mut Vec::new()).unwrap();
        repos.assert();
        assert!(stop.is_none());
        assert!(
            report.stood_down.as_deref().is_some_and(
                |reason| reason.contains("someone/elsewhere is not a connected repository")
            ),
            "{report:?}"
        );
        assert_eq!(render_push(&report), "");
        let key = Key {
            org_id: ORG.to_string(),
            repo: "someone/elsewhere".to_string(),
        };
        let held = record::load(&tmp.path().join("adr-push"), &key);
        assert_eq!(held.files["docs/adr/a.md"].outcome, Outcome::StoodDown);
        let (quiet, mocks) = quiet_server();
        let (report, _) = publish(&all_args(&quiet), root, push(10), &mut Vec::new()).unwrap();
        assert!(report.stood_down.is_none() && report.saved.is_empty());
        for mock in &mocks {
            mock.assert();
        }

        // A role that can't save: the import's 403 stands the run down too.
        std::fs::write(&a, std::fs::read_to_string(&a).unwrap() + "\n").unwrap();
        read(&mut server, &[]);
        let denied = server
            .mock("POST", IMPORT)
            .with_status(403)
            .with_body(r#"{"message":"Forbidden"}"#)
            .expect(1)
            .create();
        let args = AdrCreateArgs {
            repo: Some(REPO.to_string()),
            ..all_args(&server)
        };
        let (report, stop) = publish(&args, root, push(10), &mut Vec::new()).unwrap();
        denied.assert();
        assert!(stop.is_none());
        assert!(report.stood_down.is_some(), "{report:?}");
        let held = record::load(&tmp.path().join("adr-push"), &key);
        assert_eq!(held.files["docs/adr/a.md"].outcome, Outcome::StoodDown);
        // create --all says why instead.
        let err = publish(&args, root, create_all(true, false), &mut Vec::new())
            .map(|(_, stop)| stop)
            .unwrap();
        assert!(
            matches!(err, Some(ActualError::AdrAccessDenied { .. })),
            "{err:?}"
        );
    }

    #[test]
    fn push_stops_at_a_failed_save_and_sends_it_next_run() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempdir().unwrap();
        let _guards = sign_in(tmp.path());
        let repo = tempdir().unwrap();
        let root = repo.path();
        git_repo(root, Some(ORIGIN));
        let a = pair(
            root,
            "a.md",
            "Use Postgres",
            "MUST: Use Postgres.",
            &[],
            true,
        );
        let mut server = mockito::Server::new();
        connected(&mut server);
        read(&mut server, &[]);
        let failing = import(&mut server, root, &a, 502, r#"{"message":"Bad gateway"}"#);
        let (report, stop) = publish(&all_args(&server), root, push(10), &mut Vec::new()).unwrap();
        failing.assert();
        let stop = stop.unwrap();
        assert_eq!(stop.exit_code(), 3);
        assert_eq!(
            report.stopped.as_ref().map(|s| s.file.as_str()),
            Some("docs/adr/a.md")
        );
        assert!(render_push(&report).contains("Stopped at docs/adr/a.md"));
        assert!(!recorded(tmp.path()).contains_key("docs/adr/a.md"));
        failing.remove();
        let sent = import(&mut server, root, &a, 200, &saved(NEW_A, false));
        publish(&all_args(&server), root, push(10), &mut Vec::new()).unwrap();
        sent.assert();
    }

    #[test]
    fn push_merge_check_meets_each_answer() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempdir().unwrap();
        let _guards = sign_in(tmp.path());
        let repo = tempdir().unwrap();
        let root = repo.path();
        git_repo(root, Some(ORIGIN));
        let decided = [
            (
                "a",
                NEW_A,
                "Use Postgres for storage",
                "MUST: Store data in Postgres.",
            ),
            (
                "b",
                NEW_B,
                "Publish events to Kafka",
                "MUST: Send domain events to Kafka.",
            ),
            (
                "c",
                NEW_I,
                "Write structured logs",
                "SHOULD: Log lines as JSON objects.",
            ),
            (
                "d",
                SYNCED,
                "Cache reads in Redis",
                "MUST: Cache hot reads in Redis.",
            ),
            (
                "e",
                KAFKA,
                "Serve the API over HTTP",
                "MUST: Serve the API over HTTPS.",
            ),
        ];
        let mut server = mockito::Server::new();
        connected(&mut server);
        let first_read = read(&mut server, &[]);
        let mut texts = Vec::new();
        for (name, id, title, policy) in decided {
            let path = pair(root, &format!("{name}.md"), title, policy, &[], false);
            import(&mut server, root, &path, 200, &saved(id, false));
            texts.push(std::fs::read_to_string(&path).unwrap());
        }
        publish(&all_args(&server), root, push(10), &mut Vec::new()).unwrap();
        first_read.remove();

        // The default branch holds an unrelated file only: still nothing to
        // send.
        merge(root, &[("README.md", "# Repo\n")]);
        let (quiet, mocks) = quiet_server();
        publish(&all_args(&quiet), root, push(10), &mut Vec::new()).unwrap();
        for mock in &mocks {
            mock.assert();
        }

        // Each file reaches the default branch; d.md arrives there as a draft.
        let as_draft = texts[3].replace("status: accepted", "status: draft");
        assert_ne!(as_draft, texts[3]);
        merge(
            root,
            &[
                ("docs/adr/a.md", &texts[0]),
                ("docs/adr/b.md", &texts[1]),
                ("docs/adr/c.md", &texts[2]),
                ("docs/adr/d.md", &as_draft),
                ("docs/adr/e.md", &texts[4]),
            ],
        );
        read(
            &mut server,
            &[
                web_json(NEW_A, "Use Postgres for storage", "draft", "x"),
                web_json(NEW_B, "Publish events to Kafka", "archived-draft", "y"),
                web_json(NEW_I, "Write structured logs", "draft", "z"),
            ],
        );
        let resent = [
            import(
                &mut server,
                root,
                &root.join("docs/adr/a.md"),
                200,
                &saved(NEW_A, true),
            ),
            import(
                &mut server,
                root,
                &root.join("docs/adr/c.md"),
                400,
                r#"{"message":"The title is too short."}"#,
            ),
        ];
        let failing_publish = server
            .mock("POST", PUBLISH)
            .with_status(500)
            .with_body(r#"{"message":"down"}"#)
            .expect(1)
            .create();
        let (report, stop) = publish(&all_args(&server), root, push(2), &mut Vec::new()).unwrap();
        for mock in &resent {
            mock.assert();
        }
        failing_publish.assert();
        assert!(stop.is_none());
        // a.md's publish failed, so it waits for the next run.
        assert_eq!(report.merged.len(), 1, "{report:?}");
        assert!(report.merged[0].waits_for_merge);
        assert!(report.merged[0].outcome.publish_problem.is_some());
        // b.md's draft was archived; c.md's copy was refused.
        assert_eq!(
            report.skipped[0].reason,
            format!("already decision {NEW_B} in Actual AI (archived)")
        );
        assert_eq!(
            report.not_saved[0].reason,
            "Actual AI refused it: The title is too short."
        );
        // e.md is past the limit; d.md is a draft on the default branch.
        assert_eq!(report.left_for_next_run, ["docs/adr/e.md"]);
        let record = recorded(tmp.path());
        assert_eq!(record["docs/adr/a.md"].outcome, Outcome::WaitsForMerge);
        assert_eq!(record["docs/adr/b.md"].outcome, Outcome::Skipped);
        assert_eq!(record["docs/adr/c.md"].outcome, Outcome::Skipped);
        assert_eq!(record["docs/adr/d.md"].outcome, Outcome::WaitsForMerge);
        assert_eq!(record["docs/adr/e.md"].outcome, Outcome::WaitsForMerge);
    }

    #[test]
    fn push_merge_check_stops_or_stands_down_as_a_save_does() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempdir().unwrap();
        let _guards = sign_in(tmp.path());
        let repo = tempdir().unwrap();
        let root = repo.path();
        git_repo(root, Some(ORIGIN));
        let a = pair(
            root,
            "a.md",
            "Use Postgres",
            "MUST: Use Postgres.",
            &[],
            false,
        );
        let mut server = mockito::Server::new();
        connected(&mut server);
        read(&mut server, &[]);
        import(&mut server, root, &a, 200, &saved(NEW_A, false));
        publish(&all_args(&server), root, push(10), &mut Vec::new()).unwrap();
        let text = std::fs::read_to_string(&a).unwrap();
        merge(root, &[("docs/adr/a.md", &text)]);
        // A second file, new to the record, waits behind the merged one.
        let b = pair(
            root,
            "b.md",
            "Write structured logs",
            "MUST: Log JSON.",
            &[],
            true,
        );

        let failing = import(&mut server, root, &a, 502, r#"{"message":"Bad gateway"}"#);
        let (report, stop) = publish(&all_args(&server), root, push(10), &mut Vec::new()).unwrap();
        failing.assert();
        assert_eq!(stop.map(|e| e.exit_code()), Some(3));
        assert_eq!(
            report.stopped.as_ref().map(|s| s.file.as_str()),
            Some("docs/adr/a.md")
        );
        assert_eq!(report.not_sent, ["docs/adr/b.md"]);
        failing.remove();

        let denied = import(&mut server, root, &a, 403, r#"{"message":"Forbidden"}"#);
        let (report, stop) = publish(&all_args(&server), root, push(10), &mut Vec::new()).unwrap();
        denied.assert();
        assert!(stop.is_none());
        assert!(report.stood_down.is_some(), "{report:?}");
        let record = recorded(tmp.path());
        assert_eq!(record["docs/adr/a.md"].outcome, Outcome::WaitsForMerge);
        assert_eq!(record["docs/adr/b.md"].outcome, Outcome::StoodDown);
        assert!(b.exists());
    }

    #[test]
    fn create_all_accepts_a_merged_draft_and_sends_its_file_once() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempdir().unwrap();
        let _guards = sign_in(tmp.path());
        let repo = tempdir().unwrap();
        let root = repo.path();
        git_repo(root, Some(ORIGIN));
        let a = pair(
            root,
            "a.md",
            "Use Postgres",
            "MUST: Use Postgres.",
            &[],
            false,
        );
        let mut server = mockito::Server::new();
        connected(&mut server);
        let first_read = read(&mut server, &[]);
        let first = import(&mut server, root, &a, 200, &saved(NEW_A, false));
        publish(
            &all_args(&server),
            root,
            create_all(false, false),
            &mut Vec::new(),
        )
        .unwrap();
        first.assert();
        first_read.remove();
        first.remove();
        let text = std::fs::read_to_string(&a).unwrap();
        merge(root, &[("docs/adr/a.md", &text)]);

        read(
            &mut server,
            &[web_json(NEW_A, "Use Postgres", "draft", "Use Postgres.")],
        );
        let once = import(&mut server, root, &a, 200, &saved(NEW_A, true));
        let accept = published(&mut server, NEW_A);
        let mut progress = Vec::new();
        let (report, stop) = publish(
            &all_args(&server),
            root,
            create_all(false, false),
            &mut progress,
        )
        .unwrap();
        once.assert();
        accept.assert();
        assert!(stop.is_none());
        assert!(
            report.saved.is_empty() && report.skipped.is_empty(),
            "{report:?}"
        );
        let progress = String::from_utf8(progress).unwrap();
        assert_eq!(progress.lines().count(), 1, "{progress}");
        let text = render(&report);
        assert!(text.contains("Saved 0 of 1 decision file in"), "{text}");
        assert!(text.contains("opens a pull request"), "{text}");
        assert!(
            text.contains(&format!(
                "\nOn the default branch now (1):\n  docs/adr/a.md: saved as accepted, {NEW_A}\n"
            )),
            "{text}"
        );
    }

    #[test]
    fn a_sign_in_that_cannot_be_read_stands_push_down_and_stops_create_all() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::remove("ACTUAL_CONFIG");
        let tmp = tempdir().unwrap();
        let _g2 = EnvGuard::set("ACTUAL_CONFIG_DIR", tmp.path().to_str().unwrap());
        std::fs::write(store::credentials_path().unwrap(), "{ not: [yaml").unwrap();
        let repo = tempdir().unwrap();
        let root = repo.path();
        pair(
            root,
            "a.md",
            "Use Postgres",
            "MUST: Use Postgres.",
            &[],
            true,
        );
        let (quiet, mocks) = quiet_server();
        let (report, stop) = publish(&all_args(&quiet), root, push(10), &mut Vec::new()).unwrap();
        assert!(stop.is_none());
        assert!(
            report
                .stood_down
                .as_deref()
                .is_some_and(|reason| reason.starts_with("The sign-in couldn't be read: ")),
            "{report:?}"
        );
        let err = publish(
            &all_args(&quiet),
            root,
            create_all(true, false),
            &mut Vec::new(),
        )
        .err();
        assert!(err.is_some(), "{err:?}");
        for mock in &mocks {
            mock.assert();
        }
    }

    #[test]
    fn push_stands_down_for_a_sign_in_the_server_refuses_without_holding() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempdir().unwrap();
        let _guards = sign_in(tmp.path());
        let repo = tempdir().unwrap();
        let root = repo.path();
        git_repo(root, Some(ORIGIN));
        pair(
            root,
            "a.md",
            "Use Postgres",
            "MUST: Use Postgres.",
            &[],
            true,
        );
        let mut server = mockito::Server::new();
        connected(&mut server);
        server
            .mock("GET", READ)
            .match_query(Matcher::Any)
            .with_status(401)
            .create();
        let (report, stop) = publish(&all_args(&server), root, push(10), &mut Vec::new()).unwrap();
        assert!(stop.is_none());
        assert_eq!(report.stood_down.as_deref(), Some("Not signed in."));
        assert!(recorded(tmp.path()).is_empty(), "nothing is held");
    }

    #[test]
    fn push_with_no_origin_and_no_repository_stands_down() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempdir().unwrap();
        let _guards = sign_in(tmp.path());
        let repo = tempdir().unwrap();
        let root = repo.path();
        pair(
            root,
            "a.md",
            "Use Postgres",
            "MUST: Use Postgres.",
            &[],
            true,
        );
        let (quiet, mocks) = quiet_server();
        let (report, stop) = publish(&all_args(&quiet), root, push(10), &mut Vec::new()).unwrap();
        assert!(stop.is_none());
        assert!(
            report
                .stood_down
                .as_deref()
                .is_some_and(|reason| reason.contains("no origin remote")),
            "{report:?}"
        );
        for mock in &mocks {
            mock.assert();
        }
    }

    #[test]
    fn default_branch_reads_origin_head_then_main_then_master() {
        let repo = tempdir().unwrap();
        let root = repo.path();
        git_repo(root, Some(ORIGIN));
        assert_eq!(block(default_branch(root)), None);
        std::fs::write(root.join("f"), "x").unwrap();
        git_in(root, &["add", "f"]);
        git_in(root, &["commit", "-q", "-m", "one"]);
        git_in(root, &["update-ref", "refs/remotes/origin/master", "HEAD"]);
        assert_eq!(
            block(default_branch(root)).as_deref(),
            Some("origin/master")
        );
        git_in(root, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
        assert_eq!(block(default_branch(root)).as_deref(), Some("origin/main"));
        git_in(root, &["update-ref", "refs/remotes/origin/trunk", "HEAD"]);
        git_in(
            root,
            &[
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                "refs/remotes/origin/trunk",
            ],
        );
        assert_eq!(block(default_branch(root)).as_deref(), Some("origin/trunk"));
        assert_eq!(
            block(git(root, &["show", "origin/trunk:./f"])).as_deref(),
            Some("x")
        );
        assert_eq!(block(git(root, &["show", "origin/trunk:./missing"])), None);
    }

    #[test]
    fn names_read_as_the_repository_writes_them() {
        let root = Path::new("/repo");
        assert_eq!(
            source_path(root, &root.join("docs/adr/imported/doc-adr/0001-x.md")),
            "docs/adr/imported/doc-adr/0001-x.md"
        );
        assert_eq!(
            source_path(root, Path::new("/elsewhere/docs/adr/x.md")),
            "elsewhere/docs/adr/x.md"
        );
        assert_eq!(
            name_id(Path::new(&format!("docs/adr/{SYNCED}-use-redis.md"))).as_deref(),
            Some(SYNCED)
        );
        assert_eq!(
            name_id(Path::new(&format!("docs/adr/{}.md", SYNCED.to_uppercase()))).as_deref(),
            Some(SYNCED)
        );
        for name in ["0001-use-redis.md", "use-redis.md", "short.md"] {
            assert_eq!(name_id(Path::new(name)), None, "{name}");
        }
        assert_eq!(name_id(Path::new(&format!("{SYNCED}x.md"))), None);
    }

    #[test]
    fn own_status_keeps_the_files_status() {
        let file = |status: Option<&str>| DecisionFile {
            path: PathBuf::from("x.md"),
            meta: Some(crate::rules::types::AdrMeta {
                status: status.map(str::to_string),
                ..Default::default()
            }),
            body: String::new(),
        };
        assert_eq!(own_status(&file(Some("Draft"))), Ok(SaveStatus::Draft));
        assert_eq!(own_status(&file(Some("Proposed"))), Ok(SaveStatus::Draft));
        assert_eq!(
            own_status(&file(Some("Accepted"))),
            Ok(SaveStatus::Accepted)
        );
        assert_eq!(
            own_status(&file(Some("created-from-user"))),
            Ok(SaveStatus::Accepted)
        );
        assert_eq!(own_status(&file(None)), Ok(SaveStatus::Accepted));
        assert_eq!(
            own_status(&file(Some("rejected"))),
            Err("its status is rejected, under which it doesn't govern".to_string())
        );
    }

    #[test]
    fn render_names_each_problem_a_saved_file_left() {
        let saved = |status: &str,
                     scope: Option<&str>,
                     publish: Option<&str>,
                     problems: Vec<String>| Saved {
            file: "docs/adr/x.md".to_string(),
            already_saved: false,
            waits_for_merge: false,
            outcome: CreateOutcome {
                created: Created {
                    adr_id: "id-1".to_string(),
                    status: status.to_string(),
                    title: "T".to_string(),
                    repo_unique_id: REPO.to_string(),
                },
                files: Some(RenameOutcome {
                    renamed: Vec::new(),
                    problems,
                }),
                scope_problem: scope.map(str::to_string),
                publish_problem: publish.map(str::to_string),
                local_status_was: None,
            },
        };
        let report = BatchReport {
            repo_unique_id: Some(REPO.to_string()),
            repo_label: "o/r".to_string(),
            files: 2,
            saved: vec![
                saved(
                    "draft",
                    Some("Scope failed."),
                    None,
                    vec!["No rule file.".to_string()],
                ),
                saved("draft", None, Some("Publish failed."), Vec::new()),
            ],
            ..BatchReport::default()
        };
        assert_eq!(
            saved_text(&report.saved[0].outcome),
            "saved as a draft, id-1 (its scope wasn't set)"
        );
        assert_eq!(
            saved_text(&report.saved[1].outcome),
            "saved as a draft, id-1 (publishing it failed)"
        );
        let text = render(&report);
        assert!(
            text.contains("Saved 2 of 2 decision files in o/r: 0 accepted, 2 drafts."),
            "{text}"
        );
        assert!(!text.contains("pull request"), "{text}");
        assert!(
            text.contains("warning: docs/adr/x.md: Scope failed.\n"),
            "{text}"
        );
        assert!(
            text.contains("note: docs/adr/x.md: No rule file.\n"),
            "{text}"
        );
        assert!(
            text.contains("warning: docs/adr/x.md: Publish failed.\n"),
            "{text}"
        );
        let pushed = render_push(&report);
        assert!(
            pushed.contains("warning: docs/adr/x.md: Publish failed.\n"),
            "{pushed}"
        );
        let one = BatchReport {
            saved: vec![saved("accepted", None, None, Vec::new())],
            files: 1,
            ..report
        };
        assert!(render(&one).contains("Saved 1 of 1 decision file in o/r: 1 accepted, 0 drafts."));
        let waiting = BatchReport {
            saved: vec![
                Saved {
                    waits_for_merge: true,
                    ..saved("draft", None, None, Vec::new())
                },
                Saved {
                    waits_for_merge: true,
                    ..saved("draft", None, None, Vec::new())
                },
            ],
            ..one
        };
        assert!(render(&waiting)
            .contains("2 files marked accepted are a draft until each is on the default branch"));
    }

    #[test]
    fn create_all_takes_no_title_policy_or_status() {
        for extra in [
            vec!["--title", "T"],
            vec!["--policy", "MUST: x"],
            vec!["--context", "Why."],
            vec!["--status", "draft"],
            vec!["--file", "docs/adr/x.md"],
        ] {
            let mut argv = vec!["adr", "create", "--all"];
            argv.extend(extra.iter().copied());
            assert!(AdrArgs::try_parse_from(&argv).is_err(), "{argv:?}");
        }
        let parsed =
            AdrArgs::try_parse_from(["adr", "create", "--all", "--allow-duplicate", "--repo", "r"]);
        assert!(parsed.is_ok());
    }

    #[test]
    fn push_takes_a_limit_of_one_or_more() {
        assert!(AdrArgs::try_parse_from(["adr", "push", "--limit", "0"]).is_err());
        let parsed = AdrArgs::try_parse_from(["adr", "push", "--limit", "3", "--json"]).unwrap();
        assert!(matches!(
            parsed.action,
            crate::cli::args::AdrAction::Push(AdrPushArgs {
                limit: 3,
                api: AdrApiOptions { json: true, .. },
                ..
            })
        ));
        let parsed = AdrArgs::try_parse_from(["adr", "push"]).unwrap();
        assert!(matches!(
            parsed.action,
            crate::cli::args::AdrAction::Push(AdrPushArgs { limit: 10, .. })
        ));
    }

    #[test]
    fn exec_create_all_and_push_print_text_or_json() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::remove("ACTUAL_CONFIG");
        let tmp = tempdir().unwrap();
        let _g2 = EnvGuard::set("ACTUAL_CONFIG_DIR", tmp.path().to_str().unwrap());
        let server = mockito::Server::new();
        // The tests run from the crate root, whose docs/adr/ holds no file
        // with an ADR header, so nothing is sent either way.
        for json in [false, true] {
            let mut args = all_args(&server);
            args.api.json = json;
            let _ = exec_create_all(&args);
            let push = AdrPushArgs {
                root: None,
                repo: None,
                limit: 10,
                api: args.api.clone(),
            };
            assert!(exec_push(&push).is_ok());
        }
        // A root holding a file to send, signed out: push says nothing and
        // succeeds.
        let repo = tempdir().unwrap();
        pair(
            repo.path(),
            "a.md",
            "Use Postgres",
            "MUST: Use Postgres.",
            &[],
            true,
        );
        let push = AdrPushArgs {
            root: Some(repo.path().to_path_buf()),
            repo: None,
            limit: 10,
            api: api(&server),
        };
        assert!(exec_push(&push).is_ok());
    }
}
