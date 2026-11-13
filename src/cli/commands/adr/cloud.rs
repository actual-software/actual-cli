//! The `actual adr` commands that act on the organization's decisions in
//! Actual AI: list, show, create, update and the lifecycle actions. Each one
//! reads the decisions as the web app does and runs the web app's workflow on
//! them ([`workflow`]) before it sends a change.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use serde::Serialize;

use super::{
    content_from_flags, interactive, json_text, missing_level, pick_repo, rule_spelling, runtime,
    save_status, session, Repo, RepoPick, Session,
};
use crate::adr::context_file::{self, js_trim, ContextFile};
use crate::adr::files::{self, DecisionFile, RenameOutcome};
use crate::adr::overlap::{find_overlaps, ExistingDecision};
use crate::adr::workflow::{self, DecisionEdit, LevelEdit, Listed, PolicyEdit};
use crate::api::adr_types::{
    CreateRequest, Decision, DecisionAction, DecisionList, ImportRequest, LifecycleAction,
    NewDecision, SaveStatus,
};
use crate::cli::args::{
    AdrCreateArgs, AdrIdArgs, AdrListArgs, AdrRejectArgs, AdrShowArgs, AdrUpdateArgs,
};
use crate::cli::commands::sync::resolve_cwd;
use crate::cli::ui::theme;
use crate::error::{sanitize_server_lines, sanitize_server_text, ActualError};
use crate::rules::types::RuleLevel;

// ── reading ────────────────────────────────────────────────────────────────

/// The organization's decisions in the web app's lists, for one repository
/// when `repo` is given.
async fn read(session: &Session, repo: Option<&str>) -> Result<Vec<Listed>, ActualError> {
    let response = session.client.read_decisions(&session.org_id, repo).await?;
    Ok(workflow::listed(response))
}

/// The decisions as `list` prints them, for one repository when `repo` is
/// given.
pub(super) async fn decisions(
    session: &Session,
    repo: Option<&str>,
) -> Result<Vec<Decision>, ActualError> {
    Ok(read(session, repo)
        .await?
        .iter()
        .map(workflow::view)
        .collect())
}

/// The decision `id` names in the organization: by its own id, or by one of
/// an AI-grouped decision's rows.
async fn find(session: &Session, id: &str) -> Result<Listed, ActualError> {
    let rows = read(session, None).await?;
    workflow::find(&rows, id).cloned().ok_or_else(|| {
        ActualError::AdrNotFound(format!(
            "No decision in your organization has the id {}.",
            sanitize_server_text(id)
        ))
    })
}

// ── list ───────────────────────────────────────────────────────────────────

pub(super) fn exec_list(args: &AdrListArgs) -> Result<(), ActualError> {
    let text = runtime()?.block_on(run_list(args, &resolve_cwd()))?;
    print!("{text}");
    Ok(())
}

async fn run_list(args: &AdrListArgs, dir: &Path) -> Result<String, ActualError> {
    let session = session(&args.api).await?;
    let (repo, scope) = match pick_repo(&session, args.repo.as_deref(), dir).await? {
        RepoPick::Chosen(repo) => {
            let label = sanitize_server_text(&repo.label);
            (Some(repo.id), label)
        }
        RepoPick::Unresolved(reason) => {
            if !args.api.json {
                eprintln!(
                    "{} Listing the whole organization: {reason}.",
                    theme::hint("adr")
                );
            }
            (None, "your organization".to_string())
        }
    };
    let matching = decisions(&session, repo.as_deref())
        .await?
        .into_iter()
        .filter(|decision| {
            args.state
                .is_none_or(|state| decision.state == state.as_str())
        })
        .collect();
    let page = workflow::page(matching, args.limit, args.offset);
    Ok(if args.api.json {
        format!("{}\n", json_text(&page))
    } else {
        render_list(&page, &scope, args.offset)
    })
}

fn render_list(page: &DecisionList, scope: &str, offset: u64) -> String {
    if page.decisions.is_empty() {
        return format!("No decisions in {scope}.\n");
    }
    let shown = page.decisions.len() as u64;
    let mut out = format!(
        "{} in {scope}: {}-{} of {}\n",
        theme::heading("Decisions"),
        offset + 1,
        offset + shown,
        page.total
    );
    for decision in &page.decisions {
        let count = decision.policies.len();
        let _ = write!(
            out,
            "\n  {}\n    {} · {} · {count} {}\n",
            sanitize_server_text(&decision.title),
            sanitize_server_text(&decision.id),
            sanitize_server_text(&decision.state),
            if count == 1 { "policy" } else { "policies" },
        );
    }
    if let Some(next) = page.next_offset {
        let _ = write!(
            out,
            "\nMore: `actual adr list --offset {next}` with the same options.\n"
        );
    }
    out
}

// ── show ───────────────────────────────────────────────────────────────────

pub(super) fn exec_show(args: &AdrShowArgs) -> Result<(), ActualError> {
    let text = runtime()?.block_on(run_show(args))?;
    print!("{text}");
    Ok(())
}

async fn run_show(args: &AdrShowArgs) -> Result<String, ActualError> {
    let session = session(&args.api).await?;
    let decision = workflow::view(&find(&session, &args.id).await?);
    Ok(if args.api.json {
        format!("{}\n", json_text(&decision))
    } else {
        render_decision(&decision)
    })
}

fn render_decision(decision: &Decision) -> String {
    let clean = |text: &str| sanitize_server_text(text);
    let mut out = format!("{}\n", theme::heading(clean(&decision.title)));
    let mut row = |label: &str, value: String| {
        let _ = writeln!(out, "  {label:<12} {value}");
    };
    row("ID:", clean(&decision.id));
    row(
        "State:",
        format!("{} ({})", clean(&decision.state), clean(&decision.status)),
    );
    if decision.kind == "group" {
        row("Kind:", "grouped suggestion".to_string());
    }
    if let Some(repo) = &decision.repo_unique_id {
        row("Repository:", clean(repo));
    }
    if !decision.date.is_empty() {
        row("Date:", clean(&decision.date));
    }
    if let Some(category) = &decision.category {
        row("Category:", clean(&category.name));
    }
    row("Scope:", scope_text(decision));
    if let Some(base) = &decision.base_adr_id {
        row("Changes:", clean(base));
    }
    if let Some(context) = decision.context.as_deref().filter(|c| !c.trim().is_empty()) {
        out.push_str("\nContext\n");
        for point in sanitize_server_lines(context)
            .lines()
            .filter(|l| !l.trim().is_empty())
        {
            let _ = writeln!(out, "  - {}", point.trim());
        }
    }
    out.push_str("\nPolicies\n");
    for policy in &decision.policies {
        let _ = writeln!(
            out,
            "  {:<10} {}\n             id {}",
            rule_spelling(policy.level),
            clean(&policy.statement),
            clean(&policy.id)
        );
    }
    out
}

/// The decision's scope in words: the whole repository, or its paths.
fn scope_text(decision: &Decision) -> String {
    match &decision.scope {
        Some(scope) if !scope.paths.is_empty() => {
            let root = scope.root.trim_end_matches('/');
            scope
                .paths
                .iter()
                .map(|path| match root {
                    "." | "" => sanitize_server_text(path),
                    _ => sanitize_server_text(&format!("{root}/{path}")),
                })
                .collect::<Vec<_>>()
                .join(", ")
        }
        Some(scope) if scope.root != "." && !scope.root.is_empty() => {
            sanitize_server_text(&scope.root)
        }
        _ => "the whole repository".to_string(),
    }
}

// ── create ─────────────────────────────────────────────────────────────────

/// The decision `create` saved.
#[derive(Debug, Serialize)]
struct Created {
    adr_id: String,
    /// `accepted` or `draft`, as it was saved.
    status: String,
    title: String,
    repo_unique_id: String,
}

/// What `adr create` did, for `--json`.
#[derive(Debug, Serialize)]
struct CreateOutcome {
    #[serde(flatten)]
    created: Created,
    #[serde(skip_serializing_if = "Option::is_none")]
    files: Option<RenameOutcome>,
    #[serde(skip_serializing_if = "Option::is_none")]
    scope_problem: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    publish_problem: Option<String>,
    /// The status the pushed files had, when the save gave them another.
    #[serde(skip_serializing_if = "Option::is_none")]
    local_status_was: Option<String>,
}

pub(super) fn exec_create(args: &AdrCreateArgs) -> Result<(), ActualError> {
    let text = runtime()?.block_on(run_create(args, &resolve_cwd(), interactive()))?;
    print!("{text}");
    Ok(())
}

/// The decision `create` sends, and the local file it came from.
struct Outgoing {
    doc: ContextFile,
    file: Option<DecisionFile>,
}

/// Read what `create` would send, before anything goes out. A relative
/// `--file` is read from `dir`. Actual AI checks the decision itself when it
/// saves it.
fn outgoing(args: &AdrCreateArgs, dir: &Path) -> Result<Outgoing, ActualError> {
    let Some(path) = &args.file else {
        let title = args.title.as_deref().unwrap_or_default();
        return Ok(Outgoing {
            doc: content_from_flags(title, &args.content)?,
            file: None,
        });
    };
    let file = files::read_decision_file(&dir.join(path))?;
    let parsed = context_file::parse(&file.body).map_err(|reason| ActualError::AdrInvalid {
        reasons: vec![reason],
    })?;
    Ok(Outgoing {
        doc: parsed.doc,
        file: Some(file),
    })
}

/// The title a save stores: trimmed, or `Untitled` when blank.
fn stored_title(doc: &ContextFile) -> String {
    let title = js_trim(&doc.title);
    if title.is_empty() { "Untitled" } else { title }.to_string()
}

async fn run_create(
    args: &AdrCreateArgs,
    dir: &Path,
    interactive: bool,
) -> Result<String, ActualError> {
    let status = save_status(args.status, interactive);
    let outgoing = outgoing(args, dir)?;
    // A pushed file names its own repository through the checkout it sits in.
    let root: PathBuf = match &outgoing.file {
        Some(file) => files::root_of(&file.path).unwrap_or_else(|| dir.to_path_buf()),
        None => dir.to_path_buf(),
    };
    let session = session(&args.api).await?;
    let repo = match pick_repo(&session, args.repo.as_deref(), &root).await? {
        RepoPick::Chosen(repo) => repo,
        RepoPick::Unresolved(reason) => {
            return Err(ActualError::AdrRepoRequired(format!(
                "Couldn't tell which connected repository the decision belongs to: {reason}."
            )))
        }
    };
    if !args.allow_duplicate {
        check_for_duplicates(&session, &repo, &outgoing).await?;
    }
    let outcome = match &outgoing.file {
        None => create_from_flags(&session, &repo, &outgoing.doc, status).await?,
        Some(file) => create_from_file(&session, &repo, &root, file, &outgoing.doc, status).await?,
    };
    Ok(if args.api.json {
        format!("{}\n", json_text(&outcome))
    } else {
        render_created(&outcome, &repo)
    })
}

/// Save a decision built from flags in one request, as the web app's New
/// Decision does: a draft, or an active decision that governs at once.
async fn create_from_flags(
    session: &Session,
    repo: &Repo,
    doc: &ContextFile,
    status: SaveStatus,
) -> Result<CreateOutcome, ActualError> {
    let adr = NewDecision {
        title: doc.title.clone(),
        context: (!doc.context.is_empty()).then(|| doc.context.join("\n")),
        policies: doc.policies.iter().map(|p| p.statement.clone()).collect(),
        policy_levels: doc.policies.iter().map(|p| p.level).collect(),
        status: (status == SaveStatus::Draft).then_some("draft"),
    };
    let adr_id = session
        .client
        .create_decision(&CreateRequest {
            organization_id: session.org_id.clone(),
            repo_unique_id: repo.id.clone(),
            adr,
        })
        .await?;
    Ok(CreateOutcome {
        created: Created {
            adr_id,
            status: status.as_str().to_string(),
            title: stored_title(doc),
            repo_unique_id: repo.id.clone(),
        },
        files: None,
        scope_problem: None,
        publish_problem: None,
        local_status_was: None,
    })
}

/// Save a decision file as the web app's Import does, which keeps the whole
/// document and always saves a draft; then set the scope its header names,
/// and publish it for an accepted save. A step after the save that fails
/// leaves a draft and says how to finish. The pair is then renamed to the
/// names the repository sync gives the saved decision.
async fn create_from_file(
    session: &Session,
    repo: &Repo,
    root: &Path,
    file: &DecisionFile,
    doc: &ContextFile,
    status: SaveStatus,
) -> Result<CreateOutcome, ActualError> {
    let adr_id = session
        .client
        .import_decision(&ImportRequest {
            organization_id: session.org_id.clone(),
            repo_unique_id: repo.id.clone(),
            markdown: file.body.clone(),
        })
        .await?;
    let id = sanitize_server_text(&adr_id);
    let publish = status == SaveStatus::Accepted;
    let paths = file
        .meta
        .as_ref()
        .map(|meta| meta.paths.clone())
        .unwrap_or_default();
    let scope_problem = if paths.is_empty() {
        None
    } else {
        set_scope(session, repo, &adr_id, &paths)
            .await
            .err()
            .map(|e| {
                let flags: Vec<String> = paths
                    .iter()
                    .map(|path| format!("--scope '{path}'"))
                    .collect();
                let mut problem = format!(
                "The decision was saved, but its scope wasn't set ({e}). Run `actual adr update \
                 {id} {}` to set it.",
                flags.join(" ")
            );
                if publish {
                    let _ = write!(
                        problem,
                        " It stays a draft until then; `actual adr publish {id}` publishes it."
                    );
                }
                problem
            })
    };
    let publish_problem = if publish && scope_problem.is_none() {
        let write = workflow::publish_write(&adr_id, &session.org_id);
        session.client.write_decision(&write).await.err().map(|e| {
            format!(
                "The decision was saved as a draft, but publishing it failed ({e}). Run \
                 `actual adr publish {id}` to publish it."
            )
        })
    } else {
        None
    };
    let saved = if publish && scope_problem.is_none() && publish_problem.is_none() {
        SaveStatus::Accepted
    } else {
        SaveStatus::Draft
    };
    let title = stored_title(doc);
    let files = files::rename_pair(root, file, &adr_id, &title, saved.as_str());
    let local_status_was = file
        .meta
        .as_ref()
        .and_then(|meta| meta.status.clone())
        .filter(|was| was != saved.as_str() && !files.renamed.is_empty());
    Ok(CreateOutcome {
        created: Created {
            adr_id,
            status: saved.as_str().to_string(),
            title,
            repo_unique_id: repo.id.clone(),
        },
        files: Some(files),
        scope_problem,
        publish_problem,
        local_status_was,
    })
}

/// Give a just-saved decision the scope `paths`, as the web app's Edit does:
/// read the decision, then save it whole with the new paths.
async fn set_scope(
    session: &Session,
    repo: &Repo,
    adr_id: &str,
    paths: &[String],
) -> Result<(), ActualError> {
    let rows = read(session, Some(&repo.id)).await?;
    let listed = workflow::find(&rows, adr_id).ok_or_else(|| {
        ActualError::AdrNotFound(format!(
            "the saved decision {} isn't in the repository's decisions",
            sanitize_server_text(adr_id)
        ))
    })?;
    let edit = DecisionEdit {
        scope: Some(paths.to_vec()),
        ..DecisionEdit::default()
    };
    let write =
        workflow::edit_write(listed, &edit, &session.org_id).map_err(ActualError::AdrNotAllowed)?;
    session.client.write_decision(&write).await
}

/// Refuse to send a decision the repository already has: the file's own
/// decision, or one whose title or a policy overlaps it. Reading the
/// repository's decisions must succeed first, or nothing is sent.
async fn check_for_duplicates(
    session: &Session,
    repo: &Repo,
    outgoing: &Outgoing,
) -> Result<(), ActualError> {
    let existing = decisions(session, Some(&repo.id))
        .await
        .map_err(|e| match e {
            ActualError::NotLoggedIn | ActualError::AdrAccessDenied { .. } => e,
            other => ActualError::ApiError(format!(
                "Couldn't read the repository's decisions to check for duplicates, so nothing \
                 was sent: {other}"
            )),
        })?;
    refuse_duplicates(
        outgoing.file.as_ref().and_then(DecisionFile::adr_id),
        &outgoing.doc,
        &existing,
    )
}

fn refuse_duplicates(
    own_id: Option<&str>,
    doc: &ContextFile,
    existing: &[Decision],
) -> Result<(), ActualError> {
    if let Some(found) = own_id.and_then(|id| existing.iter().find(|d| d.id == id)) {
        let id = sanitize_server_text(&found.id);
        return Err(ActualError::AdrDuplicate {
            message: format!(
                "This file is already decision {id} in Actual AI (\"{}\", {}), so nothing was sent.",
                sanitize_server_text(&found.title),
                sanitize_server_text(&found.state)
            ),
            hint: format!("actual adr show {id}"),
        });
    }
    let candidates: Vec<ExistingDecision> = existing
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
    let policies: Vec<String> = doc.policies.iter().map(|p| p.statement.clone()).collect();
    let overlaps = find_overlaps(&doc.title, &policies, &candidates);
    if overlaps.is_empty() {
        return Ok(());
    }
    let mut message = format!(
        "This decision looks like {} the repository already has, so nothing was sent:",
        if overlaps.len() == 1 { "one" } else { "some" }
    );
    for overlap in &overlaps {
        let _ = write!(
            message,
            "\n  - {} \"{}\"",
            sanitize_server_text(&overlap.adr_id),
            sanitize_server_text(&overlap.title)
        );
    }
    Err(ActualError::AdrDuplicate {
        message,
        hint: "Run the same command with --allow-duplicate to save it anyway".to_string(),
    })
}

fn render_created(outcome: &CreateOutcome, repo: &Repo) -> String {
    let created = &outcome.created;
    let mut out = format!(
        "{} Saved \"{}\" in {} as {}: {}\n",
        theme::success(&theme::SUCCESS),
        sanitize_server_text(&created.title),
        sanitize_server_text(&repo.label),
        created.status,
        sanitize_server_text(&created.adr_id),
    );
    if created.status == "accepted" {
        out.push_str("Actual AI opens a pull request that writes its rule files.\n");
    } else {
        let _ = writeln!(
            out,
            "It governs nothing in Actual AI until `actual adr publish {}`.",
            sanitize_server_text(&created.adr_id)
        );
    }
    if let Some(files) = &outcome.files {
        for renamed in &files.renamed {
            if renamed.from != renamed.to {
                let _ = writeln!(
                    out,
                    "  renamed {} -> {}",
                    renamed.from.display(),
                    renamed.to.display()
                );
            }
        }
        for problem in &files.problems {
            let _ = writeln!(out, "  {} {problem}", theme::warning("note:"));
        }
    }
    if let Some(was) = &outcome.local_status_was {
        let _ = writeln!(
            out,
            "  The local files now say {}, as Actual AI saved it (they said {}).",
            created.status,
            sanitize_server_text(was)
        );
    }
    for problem in [&outcome.scope_problem, &outcome.publish_problem]
        .into_iter()
        .flatten()
    {
        let _ = writeln!(out, "{} {problem}", theme::warning("warning:"));
    }
    out
}

// ── update ─────────────────────────────────────────────────────────────────

/// What `adr update` did, for `--json`: the change, and the decision read
/// back after it.
#[derive(Debug, Serialize)]
struct UpdateOutcome {
    update: DecisionAction,
    #[serde(skip_serializing_if = "Option::is_none")]
    decision: Option<Decision>,
    #[serde(skip_serializing_if = "Option::is_none")]
    read_back_problem: Option<String>,
}

pub(super) fn exec_update(args: &AdrUpdateArgs) -> Result<(), ActualError> {
    let text = runtime()?.block_on(run_update(args))?;
    print!("{text}");
    Ok(())
}

/// One `--policy` value for `update`: `LEVEL: statement`, with an optional
/// `<policy id>=` in front to keep that policy.
fn parse_policy_edit(n: usize, text: &str) -> Result<PolicyEdit, String> {
    if let Some((id, rest)) = text.split_once('=') {
        let id = id.trim();
        if !id.is_empty() && !id.contains(char::is_whitespace) {
            if let Some((level, statement)) = RuleLevel::split_prefix(rest) {
                return Ok(PolicyEdit {
                    id: Some(id.to_string()),
                    level,
                    statement: statement.to_string(),
                });
            }
        }
    }
    match RuleLevel::split_prefix(text) {
        Some((level, statement)) => Ok(PolicyEdit {
            id: None,
            level,
            statement: statement.to_string(),
        }),
        None => Err(missing_level(n)),
    }
}

/// One `--level` value: `<policy id>=LEVEL`.
fn parse_level_edit(text: &str) -> Result<LevelEdit, String> {
    text.rsplit_once('=')
        .and_then(|(id, level)| {
            let id = id.trim();
            let level = RuleLevel::parse(level.trim())?;
            (!id.is_empty()).then(|| LevelEdit {
                id: id.to_string(),
                level,
            })
        })
        .ok_or_else(|| {
            format!(
                "`--level {text}` should be `<policy id>=LEVEL`, as `actual adr show` prints \
                 the ids, with LEVEL one of MUST, MUST NOT, SHOULD, SHOULD NOT or MAY."
            )
        })
}

/// The change the flags describe.
fn update_edit(args: &AdrUpdateArgs) -> Result<DecisionEdit, ActualError> {
    let mut reasons = Vec::new();
    let policies: Vec<PolicyEdit> = args
        .policies
        .iter()
        .enumerate()
        .filter_map(|(i, text)| {
            parse_policy_edit(i + 1, text)
                .map_err(|e| reasons.push(e))
                .ok()
        })
        .collect();
    let levels: Vec<LevelEdit> = args
        .levels
        .iter()
        .filter_map(|text| parse_level_edit(text).map_err(|e| reasons.push(e)).ok())
        .collect();
    if !reasons.is_empty() {
        return Err(ActualError::AdrInvalid { reasons });
    }
    let scope = if args.clear_scope {
        Some(Vec::new())
    } else {
        (!args.scope.is_empty()).then(|| args.scope.clone())
    };
    Ok(DecisionEdit {
        title: args.title.clone(),
        context: (!args.context.is_empty()).then(|| args.context.join("\n")),
        policies: (!policies.is_empty()).then_some(policies),
        levels,
        scope,
    })
}

async fn run_update(args: &AdrUpdateArgs) -> Result<String, ActualError> {
    let edit = update_edit(args)?;
    let session = session(&args.api).await?;
    let listed = find(&session, &args.id).await?;
    let write = workflow::edit_write(&listed, &edit, &session.org_id)
        .map_err(ActualError::AdrNotAllowed)?;
    session.client.write_decision(&write).await?;
    let id = sanitize_server_text(&listed.decision.id);
    // A policy-list edit can give the policies new ids, so read the
    // decision back for the next edit.
    let (decision, read_back_problem) = match find(&session, &listed.decision.id).await {
        Ok(read_back) => (Some(workflow::view(&read_back)), None),
        Err(e) => (
            None,
            Some(format!(
                "The change was saved, but reading the decision back failed ({e}). Run \
                 `actual adr show {id}` before the next edit."
            )),
        ),
    };
    let outcome = UpdateOutcome {
        update: DecisionAction {
            success: true,
            adr_id: listed.decision.id.clone(),
            reason_saved: None,
        },
        decision,
        read_back_problem,
    };
    if args.api.json {
        return Ok(format!("{}\n", json_text(&outcome)));
    }
    let mut out = format!("{} Updated {id}.\n", theme::success(&theme::SUCCESS));
    if let Some(decision) = &outcome.decision {
        out.push('\n');
        out.push_str(&render_decision(decision));
    }
    if let Some(problem) = &outcome.read_back_problem {
        let _ = writeln!(out, "{} {problem}", theme::warning("warning:"));
    }
    Ok(out)
}

// ── lifecycle and reject ───────────────────────────────────────────────────

pub(super) fn exec_lifecycle(args: &AdrIdArgs, action: LifecycleAction) -> Result<(), ActualError> {
    let text = runtime()?.block_on(run_lifecycle(args, action))?;
    print!("{text}");
    Ok(())
}

/// Send `action`'s change for the decision, once its state allows it.
async fn change(
    session: &Session,
    listed: &Listed,
    action: LifecycleAction,
) -> Result<DecisionAction, ActualError> {
    let write = workflow::lifecycle_write(listed, action, &session.org_id)
        .map_err(ActualError::AdrNotAllowed)?;
    session.client.write_decision(&write).await?;
    Ok(DecisionAction {
        success: true,
        adr_id: listed.decision.id.clone(),
        reason_saved: None,
    })
}

async fn run_lifecycle(args: &AdrIdArgs, action: LifecycleAction) -> Result<String, ActualError> {
    let session = session(&args.api).await?;
    let listed = find(&session, &args.id).await?;
    let result = change(&session, &listed, action).await?;
    if args.api.json {
        return Ok(format!("{}\n", json_text(&result)));
    }
    let id = sanitize_server_text(&result.adr_id);
    let done = match action {
        LifecycleAction::Publish => format!("Published {id}"),
        LifecycleAction::Unpublish => format!("Moved {id} back to draft"),
        LifecycleAction::Archive => {
            format!("Archived {id}. `actual adr restore {id}` brings it back")
        }
        LifecycleAction::Restore => format!("Restored {id}"),
        LifecycleAction::Accept => format!("Accepted {id}"),
        LifecycleAction::Reject => format!("Rejected {id}"),
    };
    Ok(format!("{} {done}.\n", theme::success(&theme::SUCCESS)))
}

pub(super) fn exec_reject(args: &AdrRejectArgs) -> Result<(), ActualError> {
    let text = runtime()?.block_on(run_reject(args))?;
    print!("{text}");
    Ok(())
}

/// Reject a suggestion, then save the reason with it, as the web app does.
async fn run_reject(args: &AdrRejectArgs) -> Result<String, ActualError> {
    let session = session(&args.api).await?;
    let listed = find(&session, &args.id).await?;
    let mut result = change(&session, &listed, LifecycleAction::Reject).await?;
    let feedback = workflow::rejection_feedback(&listed.decision, &session.org_id, &args.reason);
    result.reason_saved = Some(session.client.write_decision(&feedback).await.is_ok());
    if args.api.json {
        return Ok(format!("{}\n", json_text(&result)));
    }
    let mut out = format!(
        "{} Rejected {}.\n",
        theme::success(&theme::SUCCESS),
        sanitize_server_text(&result.adr_id)
    );
    if result.reason_saved == Some(false) {
        let _ = writeln!(
            out,
            "{} The reason wasn't saved; add it in the web app.",
            theme::warning("warning:")
        );
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::super::tests::{api, git_repo, sign_in, CONNECTED, ORG, REPO};
    use super::*;
    use crate::api::adr_types::{DecisionCategory, DecisionPolicy, DecisionScope};
    use crate::cli::args::{AdrApiOptions, AdrContentArgs, AdrNewArgs};
    use crate::testutil::ENV_MUTEX;
    use mockito::Matcher;
    use serde_json::json;
    use tempfile::tempdir;

    const ADR: &str = "33333333-3333-4333-8333-333333333333";
    const TODAY: &str = "2026-11-11";
    const READ: &str = "/v1/gcp/product/decisions";

    /// A person's decision as the list read returns it, with `id` its own
    /// row.
    fn web_json(id: &str, title: &str, status: &str, policies: &[(&str, &str)]) -> String {
        let rules: Vec<String> = policies
            .iter()
            .enumerate()
            .map(|(i, (level, statement))| {
                format!(
                    r#"{{"id":"rule-{}","adrId":"{id}","level":"{level}","statement":"{statement}"}}"#,
                    i + 1
                )
            })
            .collect();
        format!(
            r#"{{"id":"{id}","date":"2026-11-02T09:00:00.000Z","title":"{title}","context":"Why.",
                "status":"{status}","scope":{{"root":".","paths":[]}},"sourceRepoUniqueId":"{REPO}",
                "schemaVersion":2,"policyRules":[{}],"candidateGroupId":"{id}",
                "memberAdrIds":["{id}"],"kind":"single","primaryAdrId":"{id}"}}"#,
            rules.join(",")
        )
    }

    fn read_body(adrs: &[String]) -> String {
        format!(
            r#"{{"adrs":[{}],"suggestions":[],"version":1,"repository_summaries":[]}}"#,
            adrs.join(",")
        )
    }

    /// The read every command but `list` and `create` makes: the whole
    /// organization.
    fn org_read() -> Matcher {
        Matcher::Exact(format!("organization_id={ORG}&include_summaries=false"))
    }

    fn repo_read() -> Matcher {
        Matcher::Exact(format!(
            "organization_id={ORG}&repository_id={REPO}&include_summaries=false"
        ))
    }

    fn block<T>(future: impl std::future::Future<Output = T>) -> T {
        runtime().unwrap().block_on(future)
    }

    fn sample_decision() -> Decision {
        Decision {
            id: ADR.to_string(),
            kind: "group".to_string(),
            repo_unique_id: Some(REPO.to_string()),
            title: "Use \u{1b}[31mPostgres".to_string(),
            state: "active".to_string(),
            status: "accepted".to_string(),
            date: "2026-11-02".to_string(),
            context: Some("First.\n\nSecond.".to_string()),
            policies: vec![DecisionPolicy {
                id: format!("{ADR}:rule-1"),
                level: RuleLevel::MustNot,
                statement: "Skip it.".to_string(),
            }],
            scope: Some(DecisionScope {
                root: "backend".to_string(),
                paths: vec!["src/**".to_string(), "!src/gen/**".to_string()],
            }),
            category: Some(DecisionCategory {
                id: "c".to_string(),
                name: "Storage".to_string(),
                path: "storage".to_string(),
            }),
            base_adr_id: Some("base-1".to_string()),
        }
    }

    // ── list ──

    #[test]
    fn list_names_the_repository_it_listed() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempdir().unwrap();
        let _guards = sign_in(tmp.path());
        let mut server = mockito::Server::new();
        let mock = server
            .mock("GET", READ)
            .match_header("authorization", "Bearer tok")
            .match_query(repo_read())
            .with_status(200)
            .with_body(read_body(&[
                web_json(
                    ADR,
                    "Use Postgres",
                    "accepted",
                    &[("MUST", "Store data in it.")],
                ),
                web_json("d2", "Page reads", "draft", &[]),
                web_json("d3", "Cache reads", "created-from-user", &[]),
                web_json("d4", "Log reads", "accepted", &[]),
            ]))
            .create();
        let args = AdrListArgs {
            repo: Some(REPO.to_string()),
            state: Some(crate::api::adr_types::DecisionState::Active),
            limit: 2,
            offset: 0,
            api: api(&server),
        };
        let text = block(run_list(&args, tmp.path())).unwrap();
        mock.assert();
        assert!(text.contains(&format!("in {REPO}: 1-2 of 3")), "{text}");
        assert!(text.contains("Use Postgres") && text.contains("Cache reads"));
        assert!(!text.contains("Page reads"), "{text}");
        assert!(text.contains("1 policy"));
        assert!(text.contains("0 policies"));
        assert!(text.contains("actual adr list --offset 2"));
    }

    #[test]
    fn list_without_a_known_repository_reads_the_organization() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempdir().unwrap();
        let _guards = sign_in(tmp.path());
        let mut server = mockito::Server::new();
        let mock = server
            .mock("GET", READ)
            .match_query(org_read())
            .with_status(200)
            .with_body(read_body(&[]))
            .expect(2)
            .create();
        let mut args = AdrListArgs {
            repo: None,
            state: None,
            limit: 50,
            offset: 0,
            api: api(&server),
        };
        let text = block(run_list(&args, tmp.path())).unwrap();
        assert_eq!(text, "No decisions in your organization.\n");
        args.api.json = true;
        let json = block(run_list(&args, tmp.path())).unwrap();
        assert!(json.contains("\"total\": 0"), "{json}");
        mock.assert();
    }

    #[test]
    fn list_signed_out_stops_before_any_request() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempdir().unwrap();
        let _g1 = crate::testutil::EnvGuard::remove("ACTUAL_CONFIG");
        let _g2 = crate::testutil::EnvGuard::set("ACTUAL_CONFIG_DIR", tmp.path().to_str().unwrap());
        let args = AdrListArgs {
            repo: None,
            state: None,
            limit: 50,
            offset: 0,
            api: AdrApiOptions::default(),
        };
        assert!(matches!(
            block(run_list(&args, tmp.path())),
            Err(ActualError::NotLoggedIn)
        ));
    }

    // ── show ──

    #[test]
    fn show_prints_the_decision_or_its_json() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempdir().unwrap();
        let _guards = sign_in(tmp.path());
        let mut server = mockito::Server::new();
        server
            .mock("GET", READ)
            .match_query(org_read())
            .with_status(200)
            .with_body(read_body(&[
                web_json("d0", "Other", "accepted", &[]),
                web_json(
                    ADR,
                    "Use Postgres",
                    "draft",
                    &[("SHOULD_NOT", "Use MySQL.")],
                ),
            ]))
            .expect(2)
            .create();
        let mut args = AdrShowArgs {
            id: ADR.to_string(),
            api: api(&server),
        };
        let text = block(run_show(&args)).unwrap();
        assert!(text.contains("SHOULD NOT"), "{text}");
        assert!(text.contains(&format!("id {ADR}:rule-1")));
        assert!(text.contains("State:       draft (draft)"), "{text}");
        args.api.json = true;
        let json = block(run_show(&args)).unwrap();
        assert!(json.contains("\"level\": \"SHOULD_NOT\""), "{json}");
    }

    #[test]
    fn show_finds_an_ai_group_by_a_row_and_names_an_unknown_id() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempdir().unwrap();
        let _guards = sign_in(tmp.path());
        let mut server = mockito::Server::new();
        server
            .mock("GET", READ)
            .match_query(org_read())
            .with_status(200)
            .with_body(
                r#"{"adrs":[],"suggestions":[{"id":"g1","date":"","title":"Page reads","status":"suggested",
                    "policyRules":[{"id":"R-1","adrId":"m1","level":"MUST","statement":"Page."}],
                    "candidateGroupId":"g1","memberAdrIds":["m1"],"kind":"group","primaryAdrId":"m1"}]}"#,
            )
            .expect(2)
            .create();
        let mut args = AdrShowArgs {
            id: "m1".to_string(),
            api: api(&server),
        };
        let text = block(run_show(&args)).unwrap();
        assert!(text.contains("ID:          g1"), "{text}");
        assert!(text.contains("grouped suggestion"));
        assert!(text.contains("id m1:R-1"));
        args.id = "nope".to_string();
        let err = block(run_show(&args)).unwrap_err();
        assert!(
            matches!(&err, ActualError::AdrNotFound(m) if m == "No decision in your organization has the id nope."),
            "{err:?}"
        );
    }

    #[test]
    fn render_decision_shows_every_field_it_has() {
        let text = render_decision(&sample_decision());
        assert!(!text.contains('\u{1b}'), "server text is cleaned: {text:?}");
        for expected in [
            "Kind:        grouped suggestion",
            &format!("Repository:  {REPO}"),
            "Date:        2026-11-02",
            "Category:    Storage",
            "Scope:       backend/src/**, backend/!src/gen/**",
            "Changes:     base-1",
            "  - First.\n  - Second.\n",
            "MUST NOT   Skip it.",
        ] {
            assert!(text.contains(expected), "missing {expected:?} in {text}");
        }

        let mut bare = sample_decision();
        bare.kind = "single".to_string();
        bare.repo_unique_id = None;
        bare.date = String::new();
        bare.category = None;
        bare.base_adr_id = None;
        bare.context = Some("  ".to_string());
        bare.scope = None;
        let text = render_decision(&bare);
        assert!(
            !text.contains("Kind:") && !text.contains("Repository:") && !text.contains("Date:")
        );
        assert!(!text.contains("Context"));
        assert!(text.contains("Scope:       the whole repository"));
    }

    #[test]
    fn scope_text_reads_each_scope_shape() {
        let mut decision = sample_decision();
        decision.scope = Some(DecisionScope {
            root: ".".to_string(),
            paths: vec!["src/**".to_string()],
        });
        assert_eq!(scope_text(&decision), "src/**");
        decision.scope = Some(DecisionScope {
            root: "apps/web".to_string(),
            paths: Vec::new(),
        });
        assert_eq!(scope_text(&decision), "apps/web");
        decision.scope = Some(DecisionScope {
            root: ".".to_string(),
            paths: Vec::new(),
        });
        assert_eq!(scope_text(&decision), "the whole repository");
    }

    // ── create ──

    fn create_args(server: &mockito::Server) -> AdrCreateArgs {
        AdrCreateArgs {
            file: None,
            title: Some("Use Postgres for storage".to_string()),
            content: AdrContentArgs {
                context: vec!["We need one relational store.".to_string()],
                policies: vec!["MUST: Store relational data in Postgres.".to_string()],
            },
            status: None,
            repo: Some(REPO.to_string()),
            allow_duplicate: false,
            api: api(server),
        }
    }

    fn created_body() -> String {
        format!(
            r#"{{"success":true,"data":{{"id":"{ADR}","date":"d","title":"Use Postgres for storage","status":"draft"}}}}"#
        )
    }

    #[test]
    fn create_from_flags_checks_for_duplicates_then_sends_once() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempdir().unwrap();
        let _guards = sign_in(tmp.path());
        let mut server = mockito::Server::new();
        let list = server
            .mock("GET", READ)
            .match_query(repo_read())
            .with_status(200)
            .with_body(read_body(&[web_json(
                "d9",
                "Cache every read",
                "accepted",
                &[("MAY", "Cache reads.")],
            )]))
            .create();
        let create = server
            .mock("POST", "/v1/gcp/product/adr-create")
            .match_body(Matcher::Json(json!({
                "organization_id": ORG,
                "repo_unique_id": REPO,
                "adr": {
                    "title": "Use Postgres for storage",
                    "context": "We need one relational store.",
                    "policies": ["Store relational data in Postgres."],
                    "policyLevels": ["MUST"],
                    "status": "draft"
                }
            })))
            .with_status(200)
            .with_body(created_body())
            .expect(1)
            .create();
        let text = block(run_create(&create_args(&server), tmp.path(), false)).unwrap();
        list.assert();
        create.assert();
        assert!(text.contains(&format!("as draft: {ADR}")), "{text}");
        assert!(text.contains(&format!("actual adr publish {ADR}")));
    }

    #[test]
    fn create_at_a_terminal_saves_an_active_decision_and_prints_json() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempdir().unwrap();
        let _guards = sign_in(tmp.path());
        let mut server = mockito::Server::new();
        let create = server
            .mock("POST", "/v1/gcp/product/adr-create")
            .match_body(Matcher::Json(json!({
                "organization_id": ORG,
                "repo_unique_id": REPO,
                "adr": {
                    "title": "  ",
                    "policies": ["Store relational data in Postgres."],
                    "policyLevels": ["MUST"]
                }
            })))
            .with_status(200)
            .with_body(created_body())
            .expect(2)
            .create();
        let mut args = create_args(&server);
        args.allow_duplicate = true;
        args.title = Some("  ".to_string());
        args.content.context.clear();
        let text = block(run_create(&args, tmp.path(), true)).unwrap();
        assert!(text.contains("\"Untitled\""), "{text}");
        assert!(text.contains("as accepted"), "{text}");
        assert!(text.contains("opens a pull request"), "{text}");
        args.api.json = true;
        let json = block(run_create(&args, tmp.path(), true)).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(
            (value["adr_id"].as_str(), value["status"].as_str()),
            (Some(ADR), Some("accepted"))
        );
        assert!(value.get("files").is_none());
        create.assert();
    }

    #[test]
    fn create_refuses_a_likely_duplicate_and_sends_nothing() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempdir().unwrap();
        let _guards = sign_in(tmp.path());
        let mut server = mockito::Server::new();
        server
            .mock("GET", READ)
            .match_query(repo_read())
            .with_status(200)
            .with_body(read_body(&[web_json(
                "d1",
                "Use Postgres for storage",
                "accepted",
                &[("MUST", "Store relational data in Postgres.")],
            )]))
            .create();
        let create = server
            .mock("POST", "/v1/gcp/product/adr-create")
            .expect(0)
            .create();
        let err = block(run_create(&create_args(&server), tmp.path(), true)).unwrap_err();
        assert!(
            matches!(&err, ActualError::AdrDuplicate { message, hint }
                if message.contains("d1 \"Use Postgres for storage\"")
                    && hint.contains("--allow-duplicate")),
            "{err:?}"
        );
        create.assert();
    }

    #[test]
    fn create_fails_closed_when_the_duplicate_check_cannot_read() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempdir().unwrap();
        let _guards = sign_in(tmp.path());
        let mut server = mockito::Server::new();
        let list = server
            .mock("GET", READ)
            .match_query(Matcher::Any)
            .with_status(500)
            .create();
        let create = server
            .mock("POST", "/v1/gcp/product/adr-create")
            .expect(0)
            .create();
        let err = block(run_create(&create_args(&server), tmp.path(), true)).unwrap_err();
        assert!(
            matches!(&err, ActualError::ApiError(m) if m.contains("nothing was sent")),
            "{err:?}"
        );
        list.remove();
        server
            .mock("GET", READ)
            .match_query(Matcher::Any)
            .with_status(401)
            .create();
        let err = block(run_create(&create_args(&server), tmp.path(), true)).unwrap_err();
        assert!(matches!(err, ActualError::NotLoggedIn), "{err:?}");
        create.assert();
    }

    #[test]
    fn create_needs_a_repository() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempdir().unwrap();
        let _guards = sign_in(tmp.path());
        let server = mockito::Server::new();
        let mut args = create_args(&server);
        args.repo = None;
        let err = block(run_create(&args, tmp.path(), true)).unwrap_err();
        assert!(
            matches!(&err, ActualError::AdrRepoRequired(message) if message.contains("no origin remote")),
            "{err:?}"
        );
    }

    #[test]
    fn create_reads_its_input_before_signing_in() {
        let mut args = create_args(&mockito::Server::new());
        args.content.policies = vec!["Store data.".to_string()];
        // No sign-in is set up: the policy without a level fails first.
        let err = block(run_create(&args, Path::new("."), true)).unwrap_err();
        assert!(matches!(err, ActualError::AdrInvalid { .. }), "{err:?}");

        let tmp = tempdir().unwrap();
        let bad = tmp.path().join("bad.md");
        std::fs::write(&bad, "# Title only\n").unwrap();
        args.file = Some(bad);
        let err = block(run_create(&args, tmp.path(), true)).unwrap_err();
        assert!(
            matches!(&err, ActualError::AdrInvalid { reasons } if reasons[0].contains("No policies found")),
            "{err:?}"
        );
    }

    /// Write a pair with `adr new` into `root`, as a person at a terminal would.
    fn new_pair(root: &Path, scope: &[&str]) -> PathBuf {
        let args = AdrNewArgs {
            title: "Use Postgres for storage".to_string(),
            content: AdrContentArgs {
                context: vec!["We need one relational store.".to_string()],
                policies: vec!["MUST: Store relational data in Postgres.".to_string()],
            },
            deciders: None,
            scope: scope.iter().map(|s| s.to_string()).collect(),
            status: Some(SaveStatus::Accepted),
            root: Some(root.to_path_buf()),
            json: false,
        };
        super::super::local::write_new(&args, root, true, TODAY)
            .unwrap()
            .decision_file
    }

    fn file_args(server: &mockito::Server, file: PathBuf) -> AdrCreateArgs {
        AdrCreateArgs {
            file: Some(file),
            title: None,
            content: AdrContentArgs::default(),
            status: None,
            repo: None,
            allow_duplicate: false,
            api: api(server),
        }
    }

    fn imported_body() -> String {
        format!(r#"{{"success":true,"adr_id":"{ADR}"}}"#)
    }

    #[test]
    fn create_from_a_file_imports_it_sets_its_scope_and_publishes() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempdir().unwrap();
        let _guards = sign_in(tmp.path());
        let repo_dir = tempdir().unwrap();
        git_repo(
            repo_dir.path(),
            Some("https://github.com/actual-software/actual-cli"),
        );
        let doc = new_pair(repo_dir.path(), &["src/**"]);
        let markdown = crate::adr::files::read_decision_file(&doc).unwrap().body;
        let mut server = mockito::Server::new();
        server
            .mock("GET", "/v1/connected-repos")
            .match_query(Matcher::Any)
            .with_status(200)
            .with_body(CONNECTED)
            .create();
        let saved = web_json(
            ADR,
            "Use Postgres for storage",
            "draft",
            &[("MUST", "Store relational data in Postgres.")],
        );
        let reads = server
            .mock("GET", READ)
            .match_query(repo_read())
            .with_status(200)
            .with_body(read_body(&[saved]))
            .expect(2)
            .create();
        let import = server
            .mock("POST", "/v1/gcp/product/adr-import")
            .match_body(Matcher::Json(json!({
                "organization_id": ORG,
                "repo_unique_id": REPO,
                "markdown": markdown,
            })))
            .with_status(200)
            .with_body(imported_body())
            .expect(1)
            .create();
        let scope = server
            .mock("POST", "/v1/gcp/product/adr-group-update")
            .match_body(Matcher::PartialJson(json!({
                "group_id": ADR,
                "content": {"title": "Use Postgres for storage", "scope": {"root": ".", "paths": ["src/**"]}}
            })))
            .with_status(200)
            .with_body(format!(r#"{{"success":true,"members":[{{"adr_id":"{ADR}"}}]}}"#))
            .expect(1)
            .create();
        let publish = server
            .mock("POST", "/v1/gcp/product/adr-publish")
            .match_body(Matcher::Json(
                json!({"organization_id": ORG, "adr_id": ADR}),
            ))
            .with_status(200)
            .with_body(r#"{"success":true}"#)
            .expect(1)
            .create();
        let args = AdrCreateArgs {
            allow_duplicate: false,
            ..file_args(&server, doc.clone())
        };
        // The file's own checkout names the repository, wherever the command runs.
        let err = block(run_create(&args, tmp.path(), true)).unwrap_err();
        assert!(
            matches!(&err, ActualError::AdrDuplicate { message, .. } if message.contains("looks like one")),
            "the saved copy reads as a likely duplicate: {err:?}"
        );
        let args = AdrCreateArgs {
            allow_duplicate: true,
            ..args
        };
        let text = block(run_create(&args, tmp.path(), true)).unwrap();
        reads.assert();
        import.assert();
        scope.assert();
        publish.assert();
        assert!(!doc.exists());
        assert!(
            text.contains("in actual-software/actual-cli as accepted"),
            "{text}"
        );
        assert!(text.contains("renamed"), "{text}");
        assert!(!text.contains("warning"), "{text}");
        let moved = crate::adr::files::find_rule_file(repo_dir.path(), ADR);
        assert!(moved.is_some(), "the rule file now names the new id");
    }

    #[test]
    fn create_from_a_relative_file_with_no_terminal_saves_a_draft_and_the_files_follow() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempdir().unwrap();
        let _guards = sign_in(tmp.path());
        let repo_dir = tempdir().unwrap();
        git_repo(
            repo_dir.path(),
            Some("https://github.com/actual-software/actual-cli"),
        );
        let doc = new_pair(repo_dir.path(), &[]);
        let relative = doc.strip_prefix(repo_dir.path()).unwrap().to_path_buf();
        let mut server = mockito::Server::new();
        server
            .mock("GET", "/v1/connected-repos")
            .match_query(Matcher::Any)
            .with_status(200)
            .with_body(CONNECTED)
            .create();
        server
            .mock("GET", READ)
            .match_query(Matcher::Any)
            .with_status(200)
            .with_body(read_body(&[]))
            .expect(1)
            .create();
        let import = server
            .mock("POST", "/v1/gcp/product/adr-import")
            .with_status(200)
            .with_body(imported_body())
            .expect(1)
            .create();
        let publish = server
            .mock("POST", "/v1/gcp/product/adr-publish")
            .expect(0)
            .create();
        let text = block(run_create(
            &file_args(&server, relative),
            repo_dir.path(),
            false,
        ))
        .unwrap();
        import.assert();
        publish.assert();
        assert!(
            text.contains("in actual-software/actual-cli as draft"),
            "{text}"
        );
        assert!(
            text.contains(
                "The local files now say draft, as Actual AI saved it (they said accepted)."
            ),
            "{text}"
        );
        let rule = crate::adr::files::find_rule_file(repo_dir.path(), ADR).unwrap();
        let doc = repo_dir
            .path()
            .join("docs/adr")
            .join(crate::adr::names::decision_doc_file_name(
                ADR,
                "Use Postgres for storage",
            ));
        for moved in [doc, rule] {
            let file = crate::adr::files::read_decision_file(&moved).unwrap();
            assert_eq!(
                file.meta.unwrap().status.as_deref(),
                Some("draft"),
                "{moved:?}"
            );
        }
    }

    #[test]
    fn create_from_a_file_leaves_a_draft_when_its_scope_is_not_set() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempdir().unwrap();
        let _guards = sign_in(tmp.path());
        let root = tmp.path().join("repo");
        let doc = new_pair(&root, &["src/**", "!src/gen/**"]);
        let mut server = mockito::Server::new();
        server
            .mock("POST", "/v1/gcp/product/adr-import")
            .with_status(200)
            .with_body(imported_body())
            .create();
        let reads = server
            .mock("GET", READ)
            .match_query(repo_read())
            .with_status(200)
            .with_body(read_body(&[]))
            .expect(1)
            .create();
        let publish = server
            .mock("POST", "/v1/gcp/product/adr-publish")
            .expect(0)
            .create();
        let args = AdrCreateArgs {
            status: Some(SaveStatus::Accepted),
            repo: Some(REPO.to_string()),
            allow_duplicate: true,
            ..file_args(&server, doc)
        };
        let text = block(run_create(&args, tmp.path(), false)).unwrap();
        reads.assert();
        publish.assert();
        assert!(text.contains("as draft"), "{text}");
        assert!(
            text.contains("isn't in the repository's decisions"),
            "{text}"
        );
        assert!(
            text.contains("--scope 'src/**' --scope '!src/gen/**'"),
            "{text}"
        );
        assert!(
            text.contains(&format!("`actual adr publish {ADR}` publishes it")),
            "{text}"
        );
    }

    #[test]
    fn create_from_a_file_reports_a_publish_that_failed() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempdir().unwrap();
        let _guards = sign_in(tmp.path());
        let root = tmp.path().join("repo");
        let doc = new_pair(&root, &[]);
        let mut server = mockito::Server::new();
        server
            .mock("POST", "/v1/gcp/product/adr-import")
            .with_status(200)
            .with_body(imported_body())
            .create();
        server
            .mock("POST", "/v1/gcp/product/adr-publish")
            .with_status(500)
            .with_body(r#"{"error":"internal_error","message":"failed to publish draft ADR"}"#)
            .create();
        let mut args = AdrCreateArgs {
            status: Some(SaveStatus::Accepted),
            repo: Some(REPO.to_string()),
            allow_duplicate: true,
            ..file_args(&server, doc)
        };
        args.api.json = true;
        let json = block(run_create(&args, tmp.path(), false)).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["status"], "draft");
        assert!(
            value["publish_problem"]
                .as_str()
                .unwrap()
                .contains(&format!("Run `actual adr publish {ADR}`")),
            "{json}"
        );
        assert!(value.get("local_status_was").is_some(), "{json}");
    }

    #[test]
    fn create_refuses_a_file_that_is_already_a_decision() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempdir().unwrap();
        let _guards = sign_in(tmp.path());
        let root = tmp.path().join("repo");
        let doc = new_pair(&root, &[]);
        let own_id = crate::adr::files::read_decision_file(&doc)
            .unwrap()
            .adr_id()
            .unwrap()
            .to_string();
        let mut server = mockito::Server::new();
        server
            .mock("GET", READ)
            .match_query(Matcher::Any)
            .with_status(200)
            .with_body(read_body(&[web_json(
                &own_id,
                "Renamed since",
                "deleted",
                &[],
            )]))
            .create();
        let args = AdrCreateArgs {
            repo: Some(REPO.to_string()),
            ..file_args(&server, doc)
        };
        let err = block(run_create(&args, tmp.path(), true)).unwrap_err();
        assert!(
            matches!(&err, ActualError::AdrDuplicate { message, hint }
                if message.contains("already decision")
                    && message.contains("archived")
                    && *hint == format!("actual adr show {own_id}")),
            "{err:?}"
        );
    }

    #[test]
    fn refuse_duplicates_ignores_archived_decisions() {
        let mut archived = sample_decision();
        archived.id = "d1".to_string();
        archived.title = "Use Postgres for storage".to_string();
        archived.state = "archived".to_string();
        let doc = ContextFile {
            title: "Use Postgres for storage".to_string(),
            ..ContextFile::default()
        };
        assert!(refuse_duplicates(None, &doc, std::slice::from_ref(&archived)).is_ok());
        let mut active = archived.clone();
        active.state = "active".to_string();
        let mut second = active.clone();
        second.id = "d2".to_string();
        let err = refuse_duplicates(Some("other"), &doc, &[active, second]).err();
        assert!(
            matches!(&err, Some(ActualError::AdrDuplicate { message, .. }) if message.contains("looks like some")),
            "{err:?}"
        );
    }

    #[test]
    fn render_created_skips_unmoved_files_and_lists_problems() {
        let outcome = CreateOutcome {
            created: Created {
                adr_id: ADR.to_string(),
                status: "accepted".to_string(),
                title: "Use Postgres".to_string(),
                repo_unique_id: REPO.to_string(),
            },
            files: Some(RenameOutcome {
                renamed: vec![crate::adr::files::Renamed {
                    from: PathBuf::from("same.md"),
                    to: PathBuf::from("same.md"),
                }],
                problems: vec!["No rule file".to_string()],
            }),
            scope_problem: Some("scope".to_string()),
            publish_problem: Some("publish".to_string()),
            local_status_was: Some("draft".to_string()),
        };
        let text = render_created(
            &outcome,
            &Repo {
                id: REPO.to_string(),
                label: "o/r".to_string(),
            },
        );
        assert!(!text.contains("renamed"), "{text}");
        assert!(text.contains("No rule file"));
        assert!(
            text.contains("warning: scope\n") && text.contains("warning: publish\n"),
            "{text}"
        );
        assert!(
            text.contains(
                "The local files now say accepted, as Actual AI saved it (they said draft)."
            ),
            "{text}"
        );
    }

    // ── update ──

    #[test]
    fn parse_policy_edit_reads_an_optional_id() {
        let kept = parse_policy_edit(1, "d1:rule-1=MUST NOT: Skip it.").unwrap();
        assert_eq!(kept.id.as_deref(), Some("d1:rule-1"));
        assert_eq!(kept.level, RuleLevel::MustNot);
        assert_eq!(kept.statement, "Skip it.");
        let new = parse_policy_edit(2, "SHOULD: Set x=1 first.").unwrap();
        assert_eq!(new.id, None);
        assert_eq!(new.statement, "Set x=1 first.");
        let odd = parse_policy_edit(3, "MAY:x=1").unwrap();
        assert_eq!((odd.id, odd.level), (None, RuleLevel::May));
        assert!(parse_policy_edit(4, "=MUST: no id").is_err());
        assert!(parse_policy_edit(5, "Just prose.")
            .unwrap_err()
            .starts_with("Policy 5 "));
    }

    #[test]
    fn parse_level_edit_needs_an_id_and_a_level() {
        let edit = parse_level_edit("d1:rule-2=should not").unwrap();
        assert_eq!(edit.id, "d1:rule-2");
        assert_eq!(edit.level, RuleLevel::ShouldNot);
        for bad in ["d1:rule-2", "=MUST", "d1=LOUD"] {
            assert!(parse_level_edit(bad).unwrap_err().contains(bad), "{bad}");
        }
    }

    fn update_args(server: &mockito::Server) -> AdrUpdateArgs {
        AdrUpdateArgs {
            id: ADR.to_string(),
            title: None,
            context: Vec::new(),
            policies: Vec::new(),
            levels: Vec::new(),
            scope: Vec::new(),
            clear_scope: false,
            api: api(server),
        }
    }

    #[test]
    fn update_edit_holds_only_what_the_flags_set() {
        let server = mockito::Server::new();
        let mut args = update_args(&server);
        args.title = Some("T".to_string());
        args.context = vec!["One.".to_string(), "Two.".to_string()];
        args.policies = vec!["MUST: A.".to_string()];
        args.scope = vec!["src/**".to_string()];
        let edit = update_edit(&args).unwrap();
        assert_eq!(edit.context.as_deref(), Some("One.\nTwo."));
        assert_eq!(edit.policies.unwrap().len(), 1);
        assert!(edit.levels.is_empty());
        assert_eq!(edit.scope.unwrap(), ["src/**"]);

        let mut args = update_args(&server);
        args.clear_scope = true;
        args.levels = vec!["d1:rule-1=MAY".to_string()];
        let edit = update_edit(&args).unwrap();
        assert!(edit.scope.unwrap().is_empty());
        assert_eq!(edit.levels[0].level, RuleLevel::May);
        assert_eq!(
            (edit.title, edit.context, edit.policies),
            (None, None, None)
        );

        let mut args = update_args(&server);
        args.policies = vec!["no level".to_string()];
        args.levels = vec!["bad".to_string()];
        let err = update_edit(&args).err();
        assert!(
            matches!(&err, Some(ActualError::AdrInvalid { reasons }) if reasons.len() == 2),
            "{err:?}"
        );
    }

    #[test]
    fn update_saves_the_whole_decision_and_reads_it_back() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempdir().unwrap();
        let _guards = sign_in(tmp.path());
        let mut server = mockito::Server::new();
        let reads = server
            .mock("GET", READ)
            .match_query(org_read())
            .with_status(200)
            .with_body(read_body(&[web_json(
                ADR,
                "New title",
                "accepted",
                &[("MUST", "A.")],
            )]))
            .expect(4)
            .create();
        let update = server
            .mock("POST", "/v1/gcp/product/adr-group-update")
            .match_body(Matcher::Json(json!({
                "organization_id": ORG,
                "group_id": ADR,
                "content": {
                    "title": "New title",
                    "context": "Why.",
                    "scope": {"root": ".", "paths": []},
                    "schemaVersion": 2,
                    "policyRules": [{"adrId": ADR, "statement": "A.", "level": "MUST"}]
                }
            })))
            .with_status(200)
            .with_body(format!(
                r#"{{"success":true,"members":[{{"adr_id":"{ADR}"}}]}}"#
            ))
            .expect(2)
            .create();
        let mut args = update_args(&server);
        args.title = Some("New title".to_string());
        let text = block(run_update(&args)).unwrap();
        assert!(text.contains(&format!("Updated {ADR}.")), "{text}");
        assert!(text.contains("New title"));
        args.api.json = true;
        let json = block(run_update(&args)).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["decision"]["title"], "New title");
        assert_eq!(value["update"], json!({"success": true, "adr_id": ADR}));
        reads.assert();
        update.assert();
    }

    #[test]
    fn update_says_when_the_read_back_failed() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempdir().unwrap();
        let _guards = sign_in(tmp.path());
        let mut server = mockito::Server::new();
        let reads = std::sync::atomic::AtomicUsize::new(0);
        let listed = read_body(&[web_json(ADR, "T", "draft", &[("MUST", "A.")])]);
        // The first read finds the decision; the read back after the change fails.
        let read = server
            .mock("GET", READ)
            .match_query(org_read())
            .with_body_from_request(move |_| {
                match reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst) {
                    0 => listed.clone().into(),
                    _ => b"not json".to_vec(),
                }
            })
            .expect(2)
            .create();
        server
            .mock("POST", "/v1/gcp/product/adr-group-update")
            .with_status(200)
            .with_body(r#"{"success":true,"members":[]}"#)
            .create();
        let mut args = update_args(&server);
        args.levels = vec![format!("{ADR}:rule-1=SHOULD")];
        let text = block(run_update(&args)).unwrap();
        read.assert();
        assert!(text.contains(&format!("actual adr show {ADR}")), "{text}");
    }

    #[test]
    fn update_sends_nothing_for_a_bad_flag_or_an_archived_decision() {
        let mut args = update_args(&mockito::Server::new());
        args.policies = vec!["nope".to_string()];
        assert!(matches!(
            block(run_update(&args)),
            Err(ActualError::AdrInvalid { .. })
        ));

        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempdir().unwrap();
        let _guards = sign_in(tmp.path());
        let mut server = mockito::Server::new();
        server
            .mock("GET", READ)
            .match_query(org_read())
            .with_status(200)
            .with_body(read_body(&[web_json(
                ADR,
                "T",
                "deleted",
                &[("MUST", "A.")],
            )]))
            .create();
        let write = server
            .mock("POST", "/v1/gcp/product/adr-group-update")
            .expect(0)
            .create();
        let mut args = update_args(&server);
        args.title = Some("U".to_string());
        let err = block(run_update(&args)).unwrap_err();
        assert!(
            matches!(&err, ActualError::AdrNotAllowed(m) if m.starts_with("An archived decision")),
            "{err:?}"
        );
        write.assert();
    }

    // ── lifecycle and reject ──

    #[test]
    fn lifecycle_actions_send_their_route_and_say_what_they_did() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempdir().unwrap();
        let _guards = sign_in(tmp.path());
        let cases = [
            (
                LifecycleAction::Publish,
                "draft",
                "adr-publish",
                "Published",
            ),
            (
                LifecycleAction::Unpublish,
                "accepted",
                "adr-move-to-draft",
                "back to draft",
            ),
            (
                LifecycleAction::Archive,
                "accepted",
                "adr-archive",
                "actual adr restore",
            ),
            (
                LifecycleAction::Restore,
                "archived-draft",
                "adr-restore",
                "Restored",
            ),
            (
                LifecycleAction::Accept,
                "suggested",
                "adr-accept",
                "Accepted",
            ),
            (LifecycleAction::Reject, "updated", "adr-reject", "Rejected"),
        ];
        for (action, status, route, expected) in cases {
            let mut server = mockito::Server::new();
            server
                .mock("GET", READ)
                .match_query(org_read())
                .with_status(200)
                .with_body(read_body(&[web_json(ADR, "T", status, &[("MUST", "A.")])]))
                .create();
            let write = server
                .mock("POST", format!("/v1/gcp/product/{route}").as_str())
                .with_status(200)
                .with_body(r#"{"success":true}"#)
                .expect(2)
                .create();
            let mut args = AdrIdArgs {
                id: ADR.to_string(),
                api: api(&server),
            };
            let text = block(run_lifecycle(&args, action)).unwrap();
            assert!(text.contains(expected), "{text}");
            args.api.json = true;
            let json = block(run_lifecycle(&args, action)).unwrap();
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(&json).unwrap(),
                json!({"success": true, "adr_id": ADR})
            );
            write.assert();
        }
    }

    #[test]
    fn a_change_the_state_does_not_allow_sends_nothing() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempdir().unwrap();
        let _guards = sign_in(tmp.path());
        let mut server = mockito::Server::new();
        server
            .mock("GET", READ)
            .match_query(org_read())
            .with_status(200)
            .with_body(read_body(&[web_json(ADR, "T", "accepted", &[])]))
            .create();
        let write = server
            .mock("POST", Matcher::Regex("^/v1/gcp/product/adr-".to_string()))
            .expect(0)
            .create();
        let args = AdrIdArgs {
            id: ADR.to_string(),
            api: api(&server),
        };
        let err = block(run_lifecycle(&args, LifecycleAction::Publish)).unwrap_err();
        assert!(
            matches!(&err, ActualError::AdrNotAllowed(m) if m.ends_with("this decision is active.")),
            "{err:?}"
        );
        let reject = AdrRejectArgs {
            id: ADR.to_string(),
            reason: "r".to_string(),
            api: api(&server),
        };
        let err = block(run_reject(&reject)).unwrap_err();
        assert!(matches!(err, ActualError::AdrNotAllowed(_)), "{err:?}");
        write.assert();
    }

    #[test]
    fn reject_saves_the_reason_and_warns_when_it_could_not() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempdir().unwrap();
        let _guards = sign_in(tmp.path());
        let mut server = mockito::Server::new();
        server
            .mock("GET", READ)
            .match_query(org_read())
            .with_status(200)
            .with_body(read_body(&[web_json(
                ADR,
                "T",
                "suggested",
                &[("MUST", "A.")],
            )]))
            .create();
        let reject = server
            .mock("POST", "/v1/gcp/product/adr-reject")
            .match_body(Matcher::Json(
                json!({"organization_id": ORG, "suggestion_id": ADR}),
            ))
            .with_status(200)
            .with_body(r#"{"success":true}"#)
            .expect(3)
            .create();
        let saved = server
            .mock("POST", "/v1/gcp/product/adr-rejection-feedback")
            .match_body(Matcher::Json(
                json!({"organization_id": ORG, "adr_id": ADR, "reason": "We use MySQL."}),
            ))
            .with_status(200)
            .with_body(r#"{"success":true}"#)
            .expect(1)
            .create();
        let mut args = AdrRejectArgs {
            id: ADR.to_string(),
            reason: "We use MySQL.".to_string(),
            api: api(&server),
        };
        let text = block(run_reject(&args)).unwrap();
        assert_eq!(
            text,
            format!("{} Rejected {ADR}.\n", theme::success(&theme::SUCCESS))
        );
        saved.assert();
        saved.remove();
        server
            .mock("POST", "/v1/gcp/product/adr-rejection-feedback")
            .with_status(404)
            .with_body(r#"{"error":"not_found","message":"ADR not found or not accessible"}"#)
            .create();
        let text = block(run_reject(&args)).unwrap();
        assert!(text.contains("reason wasn't saved"), "{text}");
        args.api.json = true;
        let json = block(run_reject(&args)).unwrap();
        assert!(json.contains("\"reason_saved\": false"), "{json}");
        reject.assert();
    }

    /// Each command's exec wrapper, end to end through `adr::exec`.
    #[test]
    fn exec_wrappers_print_what_the_commands_return() {
        use clap::Parser;
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempdir().unwrap();
        let _guards = sign_in(tmp.path());
        let mut server = mockito::Server::new();
        server
            .mock("GET", READ)
            .match_query(Matcher::Any)
            .with_status(200)
            .with_body(read_body(&[
                web_json(ADR, "T", "draft", &[("MUST", "A.")]),
                web_json("s1", "S", "suggested", &[("MUST", "B.")]),
            ]))
            .create();
        server
            .mock("POST", Matcher::Regex("^/v1/gcp/product/adr-".to_string()))
            .with_status(200)
            .with_body(format!(
                r#"{{"success":true,"data":{{"id":"{ADR}","date":"d"}},"members":[]}}"#
            ))
            .create();
        let url = server.url();
        let runs: Vec<Vec<&str>> = vec![
            vec!["list", "--repo", REPO],
            vec!["show", ADR],
            vec![
                "create",
                "--title",
                "Brand new thing",
                "--context",
                "Why.",
                "--policy",
                "MUST: Do the new thing.",
                "--repo",
                REPO,
            ],
            vec!["update", ADR, "--title", "T"],
            vec!["publish", ADR],
            vec!["reject", "s1", "--reason", "r"],
        ];
        for args in runs {
            let mut argv = vec!["actual", "adr"];
            argv.extend(args.iter().copied());
            argv.extend(["--api-url", url.as_str()]);
            let adr = crate::cli::args::AdrArgs::parse_from(&argv[1..]);
            super::super::exec(&adr).unwrap_or_else(|e| panic!("{argv:?}: {e:?}"));
        }
    }
}
