//! `actual adr create --all`: every decision file under `docs/adr/` that
//! Actual AI doesn't have yet, saved one at a time the way `create --file`
//! saves one.
//!
//! A run reads the repository's decisions once, then takes the files in path
//! order. A file is skipped, and listed with the reason, when its id is
//! already one of those decisions, when its header's status is one under
//! which a decision doesn't govern, or when it looks like a decision already
//! there or saved earlier in the run. Every other file is saved with its own
//! status through `create --file`'s steps, and its pair takes the new id, so
//! the next run skips it. The run stops at the first save whose outcome is
//! unknown, and at the first error that isn't about the file being saved.

use std::fmt::Write as _;
use std::io::Write;
use std::path::Path;

use serde::Serialize;

use super::cloud::{create_from_file, decisions, CreateOutcome};
use super::{json_text, pick_repo, runtime, session, RepoPick};
use crate::adr::context_file::{self, ContextFile};
use crate::adr::files::{self, DecisionFile, DECISION_DIR};
use crate::adr::overlap::{find_overlaps, ExistingDecision};
use crate::api::adr_types::{Decision, SaveStatus};
use crate::cli::args::AdrCreateArgs;
use crate::cli::commands::sync::resolve_cwd;
use crate::cli::ui::theme;
use crate::error::{sanitize_server_text, ActualError};

/// A file saved, with what `create --file` reports for it.
#[derive(Debug, Serialize)]
struct Saved {
    file: String,
    #[serde(flatten)]
    outcome: CreateOutcome,
}

/// A file the run left alone or couldn't save, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct FileNote {
    file: String,
    reason: String,
}

/// What `create --all` did, for `--json`.
#[derive(Debug, Default, Serialize)]
struct BatchReport {
    /// The connected repository the files went to, once one was chosen.
    #[serde(skip_serializing_if = "Option::is_none")]
    repo_unique_id: Option<String>,
    #[serde(skip)]
    repo_label: String,
    /// Decision files found under `docs/adr/`, unreadable ones included.
    files: usize,
    saved: Vec<Saved>,
    skipped: Vec<FileNote>,
    not_saved: Vec<FileNote>,
    /// The file the run stopped at, and why; `not_sent` lists the files
    /// after it.
    #[serde(skip_serializing_if = "Option::is_none")]
    stopped: Option<FileNote>,
    not_sent: Vec<String>,
    /// Markdown under `docs/adr/` with no ADR header, which `create` can't
    /// send.
    other_files: Vec<String>,
}

pub(super) fn exec_create_all(args: &AdrCreateArgs) -> Result<(), ActualError> {
    let (report, stop) =
        runtime()?.block_on(run_create_all(args, &resolve_cwd(), &mut std::io::stderr()))?;
    if args.api.json {
        println!("{}", json_text(&report));
    } else {
        print!("{}", render(&report));
    }
    finish(&report, stop)
}

/// The run's outcome: the error that stopped it, else a failure when a file
/// wasn't saved.
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

/// Why a run doesn't send a file.
enum Unsent {
    Skip(String),
    NotSaved(String),
}

/// Send each decision file under `root/docs/adr/`, writing a progress line
/// per file. Returns the report and the error that stopped the run partway,
/// if one did; an error before the first file comes back on its own.
async fn run_create_all(
    args: &AdrCreateArgs,
    root: &Path,
    progress: &mut dyn Write,
) -> Result<(BatchReport, Option<ActualError>), ActualError> {
    let found = files::discover(root);
    let mut report = BatchReport {
        files: found.decisions.len() + found.unreadable.len(),
        not_saved: found
            .unreadable
            .iter()
            .map(|(path, reason)| FileNote {
                file: relative(root, path),
                reason: reason.clone(),
            })
            .collect(),
        other_files: found
            .skipped
            .iter()
            .map(|path| relative(root, path))
            .collect(),
        ..BatchReport::default()
    };
    if found.decisions.is_empty() {
        return Ok((report, None));
    }
    let session = session(&args.api).await?;
    let repo = match pick_repo(&session, args.repo.as_deref(), root).await? {
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
    report.repo_unique_id = Some(repo.id.clone());
    report.repo_label = repo.label.clone();
    let mut comparable: Vec<ExistingDecision> = existing
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
    let total = found.decisions.len();
    for (index, file) in found.decisions.iter().enumerate() {
        let name = relative(root, &file.path);
        let step = format!("[{}/{total}] {name}", index + 1);
        match plan(file, &existing, &comparable, args.allow_duplicate) {
            Err(Unsent::Skip(reason)) => {
                let _ = writeln!(progress, "{step}: skipped, {reason}");
                report.skipped.push(FileNote { file: name, reason });
            }
            Err(Unsent::NotSaved(reason)) => {
                let _ = writeln!(progress, "{step}: not saved, {reason}");
                report.not_saved.push(FileNote { file: name, reason });
            }
            Ok((doc, status)) => {
                match create_from_file(&session, &repo, root, file, &doc, status).await {
                    Ok(outcome) => {
                        let _ = writeln!(progress, "{step}: {}", saved_text(&outcome));
                        comparable.push(ExistingDecision {
                            id: outcome.created.adr_id.clone(),
                            title: doc.title.clone(),
                            policies: statements(&doc),
                        });
                        report.saved.push(Saved {
                            file: name,
                            outcome,
                        });
                    }
                    Err(ActualError::AdrInvalid { reasons }) => {
                        let reason = format!("Actual AI refused it: {}", reasons.join("; "));
                        let _ = writeln!(progress, "{step}: not saved, {reason}");
                        report.not_saved.push(FileNote { file: name, reason });
                    }
                    Err(e) => {
                        let _ = writeln!(progress, "{step}: stopped, {e}");
                        report.stopped = Some(FileNote {
                            file: name,
                            reason: e.to_string(),
                        });
                        report.not_sent = found.decisions[index + 1..]
                            .iter()
                            .map(|file| relative(root, &file.path))
                            .collect();
                        return Ok((report, Some(e)));
                    }
                }
            }
        }
    }
    Ok((report, None))
}

/// `path` relative to `root`.
fn relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .display()
        .to_string()
}

/// The decision to send from `file` and the status to save it with; or, for
/// a file Actual AI already has by its id, one that doesn't govern, one that
/// doesn't parse, or a likely duplicate without `allow_duplicate`, why not.
fn plan(
    file: &DecisionFile,
    existing: &[Decision],
    comparable: &[ExistingDecision],
    allow_duplicate: bool,
) -> Result<(ContextFile, SaveStatus), Unsent> {
    if let Some(found) = file
        .adr_id()
        .and_then(|id| existing.iter().find(|decision| decision.id == id))
    {
        return Err(Unsent::Skip(format!(
            "already decision {} in Actual AI ({})",
            sanitize_server_text(&found.id),
            sanitize_server_text(&found.state)
        )));
    }
    let status = own_status(file).map_err(Unsent::Skip)?;
    let doc = context_file::parse(&file.body)
        .map_err(Unsent::NotSaved)?
        .doc;
    let overlaps = find_overlaps(&doc.title, &statements(&doc), comparable);
    if !allow_duplicate && !overlaps.is_empty() {
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

/// The status a file is saved with, its own: a draft stays a draft, a status
/// under which the decision governs here is saved accepted, and any other
/// status keeps the file from being sent.
fn own_status(file: &DecisionFile) -> Result<SaveStatus, String> {
    let meta = file.meta.clone().unwrap_or_default();
    if file.is_draft() {
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
        if accepted > 0 {
            out.push_str(
                "Actual AI opens a pull request that writes the accepted decisions' rule files.\n",
            );
        }
    }
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
        std::fs::rename(&written.decision_file, &to).unwrap();
        to
    }

    /// A decision doc written by hand, with no rule file.
    fn doc(root: &Path, name: &str, id: &str, status: &str, body: &str) {
        let header = format!(
            "---\nversion: 2\nname: n\ndescription: d\nmetadata:\n  actual:\n    adr_id: {id}\n    \
             status: {status}\n  scope:\n    root: .\n---\n\n"
        );
        std::fs::write(root.join(DECISION_DIR).join(name), header + body).unwrap();
    }

    fn markdown(path: &Path) -> String {
        files::read_decision_file(path).unwrap().body
    }

    fn import(
        server: &mut mockito::Server,
        path: &Path,
        status: usize,
        body: &str,
    ) -> mockito::Mock {
        // Each save carries the signed-in session's token, as `create --file`'s does.
        server
            .mock("POST", IMPORT)
            .match_header("authorization", "Bearer tok")
            .match_body(Matcher::PartialJson(json!({ "markdown": markdown(path) })))
            .with_status(status)
            .with_body(body)
            .expect(1)
            .create()
    }

    fn block<T>(future: impl std::future::Future<Output = T>) -> T {
        runtime().unwrap().block_on(future)
    }

    #[test]
    fn create_all_sends_new_files_with_their_own_status_and_lists_the_rest() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempdir().unwrap();
        let _guards = sign_in(tmp.path());
        let repo = tempdir().unwrap();
        let root = repo.path();
        git_repo(root, Some("https://github.com/actual-software/actual-cli"));
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
        let decision =
            "# Use Redis\n\n## Context\n\nWhy.\n\n## Decision\n\n1. MUST: Cache reads.\n";
        doc(root, "c-synced.md", SYNCED, "accepted", decision);
        doc(root, "d-retired.md", "local-d", "superseded", decision);
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

        let mut server = mockito::Server::new();
        server
            .mock("GET", "/v1/connected-repos")
            .match_query(Matcher::Any)
            .with_status(200)
            .with_body(CONNECTED)
            .create();
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
            import(
                &mut server,
                &a,
                200,
                &format!(r#"{{"success":true,"adr_id":"{NEW_A}"}}"#),
            ),
            import(
                &mut server,
                &b,
                200,
                &format!(r#"{{"success":true,"adr_id":"{NEW_B}"}}"#),
            ),
            import(
                &mut server,
                &h,
                400,
                r#"{"message":"The title is too short."}"#,
            ),
            import(
                &mut server,
                &i,
                200,
                &format!(r#"{{"success":true,"adr_id":"{NEW_I}"}}"#),
            ),
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
        let publish = server
            .mock("POST", "/v1/gcp/product/adr-publish")
            .match_body(Matcher::Json(
                json!({"organization_id": ORG, "adr_id": NEW_A}),
            ))
            .with_status(200)
            .with_body(r#"{"success":true}"#)
            .expect(1)
            .create();

        let mut progress = Vec::new();
        let (report, stop) =
            block(run_create_all(&all_args(&server), root, &mut progress)).unwrap();

        first_read.assert();
        scope_read.assert();
        for mock in &imports {
            mock.assert();
        }
        scope.assert();
        publish.assert();
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
            lines[4].starts_with("[5/9] docs/adr/e-broken.md: not saved, No policies found."),
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
        // Each saved pair took its new id, so a second run skips it.
        assert!(!a.exists() && !b.exists() && !i.exists());
        assert!(files::find_rule_file(root, NEW_A).is_some());
        let renamed = files::discover(root);
        assert!(renamed
            .decisions
            .iter()
            .any(|file| file.adr_id() == Some(NEW_B) && file.is_draft()));

        let text = render(&report);
        assert!(
            text.contains(
                "Saved 3 of 9 decision files in actual-software/actual-cli: 1 accepted, 2 drafts."
            ),
            "{text}"
        );
        assert!(text.contains("opens a pull request"), "{text}");
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
        assert_eq!(json["repo_unique_id"], REPO);
        assert!(json.get("stopped").is_none());
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
        server
            .mock("GET", READ)
            .match_query(Matcher::Any)
            .with_status(200)
            .with_body(read_body(&[]))
            .create();
        let sent = import(&mut server, &a, 200, "not json");
        let args = AdrCreateArgs {
            repo: Some(REPO.to_string()),
            ..all_args(&server)
        };
        let mut progress = Vec::new();
        let (report, stop) = block(run_create_all(&args, root, &mut progress)).unwrap();
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
        server
            .mock("GET", READ)
            .match_query(Matcher::Any)
            .with_status(200)
            .with_body(read_body(&[web_json(
                KAFKA,
                "Use Kafka for events",
                "archived-draft",
                "x",
            )]))
            .create();
        let sent = import(
            &mut server,
            &a,
            200,
            &format!(r#"{{"success":true,"adr_id":"{NEW_A}"}}"#),
        );
        let args = AdrCreateArgs {
            repo: Some(REPO.to_string()),
            ..all_args(&server)
        };
        // An archived decision is no duplicate.
        let (report, _) = block(run_create_all(&args, root, &mut Vec::new())).unwrap();
        assert_eq!(report.saved.len(), 1);
        sent.assert();

        // The saved file now carries its new id, which the next read lists.
        let b = pair(
            root,
            "b.md",
            "Use Kafka for events",
            "MUST: Publish events to Kafka.",
            &[],
            true,
        );
        server
            .mock("GET", READ)
            .match_query(Matcher::Any)
            .with_status(200)
            .with_body(read_body(&[
                web_json(KAFKA, "Use Kafka for events", "draft", "x"),
                web_json(
                    NEW_A,
                    "Use Kafka for events",
                    "draft",
                    "Publish events to Kafka.",
                ),
            ]))
            .create();
        let again = import(
            &mut server,
            &b,
            200,
            &format!(r#"{{"success":true,"adr_id":"{NEW_B}"}}"#),
        );
        let args = AdrCreateArgs {
            allow_duplicate: true,
            ..args
        };
        let (report, _) = block(run_create_all(&args, root, &mut Vec::new())).unwrap();
        again.assert();
        assert_eq!(report.saved.len(), 1, "{report:?}");
        assert_eq!(report.skipped.len(), 1, "{report:?}");
        assert!(report.skipped[0].reason.starts_with("already decision"));
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
        let (report, stop) =
            block(run_create_all(&all_args(&server), root, &mut Vec::new())).unwrap();
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
        let err = block(run_create_all(&all_args(&server), root, &mut Vec::new())).err();
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
        let err = block(run_create_all(&all_args(&server), root, &mut Vec::new())).err();
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
        let err = block(run_create_all(&args, root, &mut Vec::new())).err();
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
        let err = block(run_create_all(&args, root, &mut Vec::new())).err();
        assert!(matches!(err, Some(ActualError::NotLoggedIn)), "{err:?}");
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
        let one = BatchReport {
            saved: vec![saved("accepted", None, None, Vec::new())],
            files: 1,
            ..report
        };
        assert!(render(&one).contains("Saved 1 of 1 decision file in o/r: 1 accepted, 0 drafts."));
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
    fn exec_create_all_prints_text_or_json() {
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
        }
    }
}
