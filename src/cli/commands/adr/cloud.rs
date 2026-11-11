//! The `actual adr` commands that act on the organization's decisions in
//! Actual AI: list, show, create, update and the lifecycle actions.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use serde::Serialize;

use super::{
    content_from_flags, interactive, json_text, missing_level, pick_repo, rule_spelling, runtime,
    save_status, session, today, Repo, RepoPick, Session,
};
use crate::adr::context_file::{self, ContextFile};
use crate::adr::files::{self, DecisionFile, RenameOutcome};
use crate::adr::overlap::{find_overlaps, ExistingDecision};
use crate::adr::save_check::{check_document, check_markdown, MAX_CONTEXT_FILE_UNITS};
use crate::api::adr_types::{
    CreateDecisionRequest, CreatedDecision, Decision, DecisionAction, DecisionList, LevelEdit,
    LifecycleAction, PolicyEdit, SaveStatus, ScopeEdit, UpdateDecisionRequest,
};
use crate::cli::args::{
    AdrCreateArgs, AdrIdArgs, AdrListArgs, AdrRejectArgs, AdrShowArgs, AdrUpdateArgs,
};
use crate::cli::commands::sync::resolve_cwd;
use crate::cli::ui::theme;
use crate::error::{sanitize_server_lines, sanitize_server_text, ActualError};
use crate::rules::types::RuleLevel;

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
    let page = session
        .client
        .list_decisions(
            &session.org_id,
            repo.as_deref(),
            args.state,
            args.limit,
            args.offset,
        )
        .await?;
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
    let decision = session
        .client
        .show_decision(&session.org_id, &args.id, None)
        .await?;
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

/// What `adr create` did, for `--json`.
#[derive(Debug, Serialize)]
struct CreateOutcome {
    #[serde(flatten)]
    created: CreatedDecision,
    #[serde(skip_serializing_if = "Option::is_none")]
    files: Option<RenameOutcome>,
    #[serde(skip_serializing_if = "Option::is_none")]
    scope_problem: Option<String>,
    /// The status the pushed files had, when the save gave them another.
    #[serde(skip_serializing_if = "Option::is_none")]
    local_status_was: Option<String>,
}

pub(super) fn exec_create(args: &AdrCreateArgs) -> Result<(), ActualError> {
    let text = runtime()?.block_on(run_create(args, &resolve_cwd(), interactive(), &today()))?;
    print!("{text}");
    Ok(())
}

/// The decision `create` sends: its Context File markdown, the decision it
/// reads as, and the local file it came from.
struct Outgoing {
    markdown: String,
    doc: ContextFile,
    file: Option<DecisionFile>,
}

/// Read and check what `create` would send, before anything goes out. A
/// relative `--file` is read from `dir`.
fn outgoing(
    args: &AdrCreateArgs,
    dir: &Path,
    as_draft: bool,
    today: &str,
) -> Result<Outgoing, ActualError> {
    let Some(path) = &args.file else {
        let title = args.title.as_deref().unwrap_or_default();
        let doc = content_from_flags(title, &args.content)?;
        let doc = check_document(&doc, as_draft, today)
            .map_err(|reasons| ActualError::AdrInvalid { reasons })?;
        return Ok(Outgoing {
            markdown: context_file::render(&doc),
            doc,
            file: None,
        });
    };
    let file = files::read_decision_file(&dir.join(path))?;
    if file.body.encode_utf16().count() > MAX_CONTEXT_FILE_UNITS {
        return Err(ActualError::AdrInvalid {
            reasons: vec![format!(
                "{} is over 100 KB, the most a decision can hold.",
                path.display()
            )],
        });
    }
    let doc = check_markdown(&file.body, as_draft, today)
        .result
        .map_err(|reasons| ActualError::AdrInvalid { reasons })?;
    Ok(Outgoing {
        markdown: file.body.clone(),
        doc,
        file: Some(file),
    })
}

async fn run_create(
    args: &AdrCreateArgs,
    dir: &Path,
    interactive: bool,
    today: &str,
) -> Result<String, ActualError> {
    let status = save_status(args.status, interactive);
    let outgoing = outgoing(args, dir, status == SaveStatus::Draft, today)?;
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

    let created = session
        .client
        .create_decision(&CreateDecisionRequest {
            org_id: session.org_id.clone(),
            repo_unique_id: repo.id.clone(),
            status,
            markdown: outgoing.markdown,
        })
        .await?;

    let files = outgoing.file.as_ref().map(|file| {
        files::rename_pair(
            &root,
            file,
            &created.adr_id,
            &created.title,
            &created.status,
        )
    });
    let local_status_was = outgoing
        .file
        .as_ref()
        .and_then(|file| file.meta.as_ref()?.status.clone())
        .filter(|was| {
            *was != created.status && files.as_ref().is_some_and(|f| !f.renamed.is_empty())
        });
    let scope_paths = outgoing
        .file
        .as_ref()
        .and_then(|file| file.meta.as_ref())
        .map(|meta| meta.paths.clone())
        .unwrap_or_default();
    let scope_problem = if scope_paths.is_empty() {
        None
    } else {
        let request = UpdateDecisionRequest {
            org_id: session.org_id.clone(),
            scope: Some(ScopeEdit {
                paths: scope_paths.clone(),
            }),
            ..UpdateDecisionRequest::default()
        };
        session
            .client
            .update_decision(&created.adr_id, &request)
            .await
            .err()
            .map(|e| {
                format!(
                    "The decision was saved, but its scope wasn't set ({e}). Run `actual adr \
                     update {} {}` to set it.",
                    created.adr_id,
                    scope_paths
                        .iter()
                        .map(|path| format!("--scope '{path}'"))
                        .collect::<Vec<_>>()
                        .join(" ")
                )
            })
    };

    let outcome = CreateOutcome {
        created,
        files,
        scope_problem,
        local_status_was,
    };
    Ok(if args.api.json {
        format!("{}\n", json_text(&outcome))
    } else {
        render_created(&outcome, &repo)
    })
}

/// Refuse to send a decision the repository already has: the file's own
/// decision, or one whose title or a policy overlaps it. Reading the
/// repository's decisions must succeed first, or nothing is sent.
async fn check_for_duplicates(
    session: &Session,
    repo: &Repo,
    outgoing: &Outgoing,
) -> Result<(), ActualError> {
    let existing = session
        .client
        .list_all_decisions(&session.org_id, Some(&repo.id))
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
        sanitize_server_text(&created.status),
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
            sanitize_server_text(&created.status),
            sanitize_server_text(was)
        );
    }
    if let Some(problem) = &outcome.scope_problem {
        let _ = writeln!(out, "{} {problem}", theme::warning("warning:"));
    }
    out
}

// ── update ─────────────────────────────────────────────────────────────────

/// What `adr update` did, for `--json`: the route's answer and the decision
/// read back after it.
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

/// The update the flags describe, minus the organization.
fn update_request(args: &AdrUpdateArgs) -> Result<UpdateDecisionRequest, ActualError> {
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
        Some(ScopeEdit { paths: Vec::new() })
    } else {
        (!args.scope.is_empty()).then(|| ScopeEdit {
            paths: args.scope.clone(),
        })
    };
    Ok(UpdateDecisionRequest {
        org_id: String::new(),
        title: args.title.clone(),
        context: (!args.context.is_empty()).then(|| args.context.join("\n")),
        policies: (!policies.is_empty()).then_some(policies),
        levels: (!levels.is_empty()).then_some(levels),
        scope,
    })
}

async fn run_update(args: &AdrUpdateArgs) -> Result<String, ActualError> {
    let mut request = update_request(args)?;
    let session = session(&args.api).await?;
    request.org_id = session.org_id.clone();
    let update = session.client.update_decision(&args.id, &request).await?;
    // A policy-list edit can give the policies new ids, so read the
    // decision back for the next edit.
    let (decision, read_back_problem) = match session
        .client
        .show_decision(&session.org_id, &update.adr_id, None)
        .await
    {
        Ok(decision) => (Some(decision), None),
        Err(e) => (
            None,
            Some(format!(
                "The change was saved, but reading the decision back failed ({e}). Run \
                 `actual adr show {}` before the next edit.",
                sanitize_server_text(&update.adr_id)
            )),
        ),
    };
    let outcome = UpdateOutcome {
        update,
        decision,
        read_back_problem,
    };
    if args.api.json {
        return Ok(format!("{}\n", json_text(&outcome)));
    }
    let mut out = format!(
        "{} Updated {}.{}\n",
        theme::success(&theme::SUCCESS),
        sanitize_server_text(&outcome.update.adr_id),
        sync_note(&outcome.update)
    );
    if let Some(decision) = &outcome.decision {
        out.push('\n');
        out.push_str(&render_decision(decision));
    }
    if let Some(problem) = &outcome.read_back_problem {
        let _ = writeln!(out, "{} {problem}", theme::warning("warning:"));
    }
    Ok(out)
}

/// ` Actual AI is updating the repository's rule files.` when the change
/// started a sync.
fn sync_note(action: &DecisionAction) -> &'static str {
    if action.sync_requested() {
        " Actual AI is updating the repository's rule files."
    } else {
        ""
    }
}

// ── lifecycle and reject ───────────────────────────────────────────────────

pub(super) fn exec_lifecycle(args: &AdrIdArgs, action: LifecycleAction) -> Result<(), ActualError> {
    let text = runtime()?.block_on(run_lifecycle(args, action))?;
    print!("{text}");
    Ok(())
}

async fn run_lifecycle(args: &AdrIdArgs, action: LifecycleAction) -> Result<String, ActualError> {
    let session = session(&args.api).await?;
    let result = session
        .client
        .decision_lifecycle(&args.id, action, &session.org_id)
        .await?;
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
    };
    Ok(format!(
        "{} {done}.{}\n",
        theme::success(&theme::SUCCESS),
        sync_note(&result)
    ))
}

pub(super) fn exec_reject(args: &AdrRejectArgs) -> Result<(), ActualError> {
    let text = runtime()?.block_on(run_reject(args))?;
    print!("{text}");
    Ok(())
}

async fn run_reject(args: &AdrRejectArgs) -> Result<String, ActualError> {
    let session = session(&args.api).await?;
    let result = session
        .client
        .reject_decision(&args.id, &session.org_id, &args.reason)
        .await?;
    if args.api.json {
        return Ok(format!("{}\n", json_text(&result)));
    }
    let mut out = format!(
        "{} Rejected {}.{}\n",
        theme::success(&theme::SUCCESS),
        sanitize_server_text(&result.adr_id),
        sync_note(&result)
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
    use tempfile::tempdir;

    const ADR: &str = "33333333-3333-4333-8333-333333333333";
    const TODAY: &str = "2026-11-11";

    fn decision_json(id: &str, title: &str, state: &str, policies: &[(&str, &str)]) -> String {
        let policies: Vec<String> = policies
            .iter()
            .enumerate()
            .map(|(i, (level, statement))| {
                format!(
                    r#"{{"id":"{id}:rule-{}","level":"{level}","statement":"{statement}"}}"#,
                    i + 1
                )
            })
            .collect();
        format!(
            r#"{{"id":"{id}","kind":"single","repo_unique_id":"{REPO}","title":"{title}","state":"{state}",
                "status":"accepted","date":"2026-11-02T09:00:00.000Z","context":"Why.",
                "policies":[{}],"scope":{{"root":".","paths":[]}},"category":null,"base_adr_id":null}}"#,
            policies.join(",")
        )
    }

    fn list_body(decisions: &[String], next: Option<u64>) -> String {
        format!(
            r#"{{"decisions":[{}],"total":{},"next_offset":{}}}"#,
            decisions.join(","),
            decisions.len(),
            next.map_or("null".to_string(), |n| n.to_string())
        )
    }

    fn action_body(sync: &str) -> String {
        format!(r#"{{"success":true,"adr_id":"{ADR}","sync":"{sync}"}}"#)
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
            .mock("GET", "/v1/cli/adrs")
            .match_query(Matcher::Exact(format!(
                "org_id={ORG}&repo_unique_id={REPO}&state=active&limit=2&offset=0"
            )))
            .with_status(200)
            .with_body(list_body(
                &[
                    decision_json(
                        ADR,
                        "Use Postgres",
                        "active",
                        &[("MUST", "Store data in it.")],
                    ),
                    decision_json("d2", "Page reads", "active", &[]),
                ],
                Some(2),
            ))
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
        assert!(text.contains(&format!("in {REPO}: 1-2 of 2")), "{text}");
        assert!(text.contains("Use Postgres"));
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
            .mock("GET", "/v1/cli/adrs")
            .match_query(Matcher::Exact(format!("org_id={ORG}&limit=50&offset=0")))
            .with_status(200)
            .with_body(list_body(&[], None))
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
            .mock("GET", format!("/v1/cli/adrs/{ADR}").as_str())
            .match_query(Matcher::Exact(format!("org_id={ORG}")))
            .with_status(200)
            .with_body(format!(
                r#"{{"decision":{}}}"#,
                decision_json(
                    ADR,
                    "Use Postgres",
                    "draft",
                    &[("SHOULD_NOT", "Use MySQL.")]
                )
            ))
            .expect(2)
            .create();
        let mut args = AdrShowArgs {
            id: ADR.to_string(),
            api: api(&server),
        };
        let text = block(run_show(&args)).unwrap();
        assert!(text.contains("SHOULD NOT"), "{text}");
        assert!(text.contains(&format!("id {ADR}:rule-1")));
        args.api.json = true;
        let json = block(run_show(&args)).unwrap();
        assert!(json.contains("\"level\": \"SHOULD_NOT\""), "{json}");
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
                deciders: None,
            },
            status: None,
            repo: Some(REPO.to_string()),
            allow_duplicate: false,
            api: api(server),
        }
    }

    fn created_body(status: &str) -> String {
        format!(
            r#"{{"adr_id":"{ADR}","status":"{status}","title":"Use Postgres for storage","repo_unique_id":"{REPO}"}}"#
        )
    }

    #[test]
    fn create_from_flags_checks_for_duplicates_then_sends_once() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempdir().unwrap();
        let _guards = sign_in(tmp.path());
        let mut server = mockito::Server::new();
        let list = server
            .mock("GET", "/v1/cli/adrs")
            .match_query(Matcher::UrlEncoded("repo_unique_id".into(), REPO.into()))
            .with_status(200)
            .with_body(list_body(
                &[decision_json(
                    "d9",
                    "Cache every read",
                    "active",
                    &[("MAY", "Cache reads.")],
                )],
                None,
            ))
            .create();
        let create = server
            .mock("POST", "/v1/cli/adrs")
            .match_body(Matcher::PartialJson(serde_json::json!({
                "org_id": ORG,
                "repo_unique_id": REPO,
                "status": "draft",
            })))
            .with_status(200)
            .with_body(created_body("draft"))
            .expect(1)
            .create();
        let text = block(run_create(&create_args(&server), tmp.path(), false, TODAY)).unwrap();
        list.assert();
        create.assert();
        assert!(text.contains(&format!("as draft: {ADR}")), "{text}");
        assert!(text.contains(&format!("actual adr publish {ADR}")));
    }

    #[test]
    fn create_at_a_terminal_saves_accepted_and_prints_json() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempdir().unwrap();
        let _guards = sign_in(tmp.path());
        let mut server = mockito::Server::new();
        server
            .mock("POST", "/v1/cli/adrs")
            .match_body(Matcher::PartialJson(
                serde_json::json!({"status": "accepted"}),
            ))
            .with_status(200)
            .with_body(created_body("accepted"))
            .create();
        let mut args = create_args(&server);
        args.allow_duplicate = true;
        let text = block(run_create(&args, tmp.path(), true, TODAY)).unwrap();
        assert!(text.contains("opens a pull request"), "{text}");
        args.api.json = true;
        let json = block(run_create(&args, tmp.path(), true, TODAY)).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["adr_id"], ADR);
        assert!(value.get("files").is_none());
    }

    #[test]
    fn create_refuses_a_likely_duplicate_and_sends_nothing() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempdir().unwrap();
        let _guards = sign_in(tmp.path());
        let mut server = mockito::Server::new();
        server
            .mock("GET", "/v1/cli/adrs")
            .match_query(Matcher::Any)
            .with_status(200)
            .with_body(list_body(
                &[decision_json(
                    "d1",
                    "Use Postgres for storage",
                    "active",
                    &[("MUST", "Store relational data in Postgres.")],
                )],
                None,
            ))
            .create();
        let create = server.mock("POST", "/v1/cli/adrs").expect(0).create();
        let err = block(run_create(&create_args(&server), tmp.path(), true, TODAY)).unwrap_err();
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
            .mock("GET", "/v1/cli/adrs")
            .match_query(Matcher::Any)
            .with_status(500)
            .create();
        let create = server.mock("POST", "/v1/cli/adrs").expect(0).create();
        let err = block(run_create(&create_args(&server), tmp.path(), true, TODAY)).unwrap_err();
        assert!(
            matches!(&err, ActualError::ApiError(m) if m.contains("nothing was sent")),
            "{err:?}"
        );
        list.remove();
        server
            .mock("GET", "/v1/cli/adrs")
            .match_query(Matcher::Any)
            .with_status(401)
            .create();
        let err = block(run_create(&create_args(&server), tmp.path(), true, TODAY)).unwrap_err();
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
        let err = block(run_create(&args, tmp.path(), true, TODAY)).unwrap_err();
        assert!(
            matches!(&err, ActualError::AdrRepoRequired(message) if message.contains("no origin remote")),
            "{err:?}"
        );
    }

    #[test]
    fn create_checks_the_decision_before_signing_in() {
        let mut args = create_args(&mockito::Server::new());
        args.content.context.clear();
        // No sign-in is set up: the save check fails first.
        let err = block(run_create(&args, Path::new("."), true, TODAY)).unwrap_err();
        assert!(matches!(err, ActualError::AdrInvalid { .. }), "{err:?}");
    }

    /// Write a pair with `adr new` into `root`, as a person at a terminal would.
    fn new_pair(root: &Path, scope: &[&str]) -> PathBuf {
        let args = AdrNewArgs {
            title: "Use Postgres for storage".to_string(),
            content: AdrContentArgs {
                context: vec!["We need one relational store.".to_string()],
                policies: vec!["MUST: Store relational data in Postgres.".to_string()],
                deciders: None,
            },
            scope: scope.iter().map(|s| s.to_string()).collect(),
            status: Some(SaveStatus::Accepted),
            root: Some(root.to_path_buf()),
            json: false,
        };
        super::super::local::write_new(&args, root, true, TODAY)
            .unwrap()
            .decision_file
    }

    #[test]
    fn create_from_a_file_renames_the_pair_and_sends_its_scope() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempdir().unwrap();
        let _guards = sign_in(tmp.path());
        let repo_dir = tempdir().unwrap();
        git_repo(
            repo_dir.path(),
            Some("https://github.com/actual-software/actual-cli"),
        );
        let doc = new_pair(repo_dir.path(), &["src/**"]);
        let mut server = mockito::Server::new();
        server
            .mock("GET", "/v1/connected-repos")
            .match_query(Matcher::Any)
            .with_status(200)
            .with_body(CONNECTED)
            .create();
        server
            .mock("GET", "/v1/cli/adrs")
            .match_query(Matcher::Any)
            .with_status(200)
            .with_body(list_body(&[], None))
            .create();
        let create = server
            .mock("POST", "/v1/cli/adrs")
            .with_status(200)
            .with_body(created_body("accepted"))
            .expect(1)
            .create();
        let scope = server
            .mock("POST", format!("/v1/cli/adrs/{ADR}/update").as_str())
            .match_body(Matcher::Json(
                serde_json::json!({"org_id": ORG, "scope": {"paths": ["src/**"]}}),
            ))
            .with_status(200)
            .with_body(action_body("requested"))
            .create();
        let args = AdrCreateArgs {
            file: Some(doc.clone()),
            title: None,
            content: AdrContentArgs::default(),
            status: None,
            repo: None,
            allow_duplicate: false,
            api: api(&server),
        };
        // The file's own checkout names the repository, wherever the command runs.
        let text = block(run_create(&args, tmp.path(), true, TODAY)).unwrap();
        create.assert();
        scope.assert();
        assert!(!doc.exists());
        assert!(
            text.contains("in actual-software/actual-cli as accepted"),
            "{text}"
        );
        assert!(text.contains("renamed"), "{text}");
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
            .mock("GET", "/v1/cli/adrs")
            .match_query(Matcher::Any)
            .with_status(200)
            .with_body(list_body(&[], None))
            .create();
        let create = server
            .mock("POST", "/v1/cli/adrs")
            .match_body(Matcher::PartialJson(serde_json::json!({"status": "draft"})))
            .with_status(200)
            .with_body(created_body("draft"))
            .expect(1)
            .create();
        let args = AdrCreateArgs {
            file: Some(relative),
            title: None,
            content: AdrContentArgs::default(),
            status: None,
            repo: None,
            allow_duplicate: false,
            api: api(&server),
        };
        let text = block(run_create(&args, repo_dir.path(), false, TODAY)).unwrap();
        create.assert();
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
    fn create_from_a_file_reports_a_scope_it_could_not_set() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempdir().unwrap();
        let _guards = sign_in(tmp.path());
        let root = tmp.path().join("repo");
        let doc = new_pair(&root, &["src/**", "!src/gen/**"]);
        let mut server = mockito::Server::new();
        server
            .mock("POST", "/v1/cli/adrs")
            .with_status(200)
            .with_body(created_body("accepted"))
            .create();
        server
            .mock("POST", format!("/v1/cli/adrs/{ADR}/update").as_str())
            .with_status(409)
            .with_body(r#"{"error":"conflict","message":"no"}"#)
            .create();
        let args = AdrCreateArgs {
            file: Some(doc),
            title: None,
            content: AdrContentArgs::default(),
            status: Some(SaveStatus::Accepted),
            repo: Some(REPO.to_string()),
            allow_duplicate: true,
            api: api(&server),
        };
        let text = block(run_create(&args, tmp.path(), false, TODAY)).unwrap();
        assert!(
            text.contains("--scope 'src/**' --scope '!src/gen/**'"),
            "{text}"
        );
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
            .mock("GET", "/v1/cli/adrs")
            .match_query(Matcher::Any)
            .with_status(200)
            .with_body(list_body(
                &[decision_json(&own_id, "Renamed since", "archived", &[])],
                None,
            ))
            .create();
        let args = AdrCreateArgs {
            file: Some(doc),
            title: None,
            content: AdrContentArgs::default(),
            status: None,
            repo: Some(REPO.to_string()),
            allow_duplicate: false,
            api: api(&server),
        };
        let err = block(run_create(&args, tmp.path(), true, TODAY)).unwrap_err();
        assert!(
            matches!(&err, ActualError::AdrDuplicate { message, hint }
                if message.contains("already decision")
                    && *hint == format!("actual adr show {own_id}")),
            "{err:?}"
        );
    }

    #[test]
    fn create_refuses_a_file_that_fails_the_check_or_is_too_big() {
        let tmp = tempdir().unwrap();
        let bad = tmp.path().join("bad.md");
        std::fs::write(&bad, "# Title only\n").unwrap();
        let server = mockito::Server::new();
        let mut args = AdrCreateArgs {
            file: Some(bad.clone()),
            title: None,
            content: AdrContentArgs::default(),
            status: None,
            repo: Some(REPO.to_string()),
            allow_duplicate: true,
            api: api(&server),
        };
        assert!(matches!(
            block(run_create(&args, tmp.path(), true, TODAY)),
            Err(ActualError::AdrInvalid { .. })
        ));
        let big = tmp.path().join("big.md");
        std::fs::write(&big, "x".repeat(MAX_CONTEXT_FILE_UNITS + 1)).unwrap();
        args.file = Some(big);
        let err = block(run_create(&args, tmp.path(), true, TODAY)).err();
        assert!(
            matches!(&err, Some(ActualError::AdrInvalid { reasons }) if reasons[0].contains("over 100 KB")),
            "{err:?}"
        );
    }

    #[test]
    fn refuse_duplicates_ignores_archived_decisions() {
        let archived: Decision = serde_json::from_str(&decision_json(
            "d1",
            "Use Postgres for storage",
            "archived",
            &[("MUST", "Store relational data in Postgres.")],
        ))
        .unwrap();
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
            created: serde_json::from_str(&created_body("accepted")).unwrap(),
            files: Some(RenameOutcome {
                renamed: vec![crate::adr::files::Renamed {
                    from: PathBuf::from("same.md"),
                    to: PathBuf::from("same.md"),
                }],
                problems: vec!["No rule file".to_string()],
            }),
            scope_problem: Some("scope".to_string()),
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
        assert!(text.contains("scope"));
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
    fn update_request_sends_only_what_the_flags_set() {
        let server = mockito::Server::new();
        let mut args = update_args(&server);
        args.title = Some("T".to_string());
        args.context = vec!["One.".to_string(), "Two.".to_string()];
        args.policies = vec!["MUST: A.".to_string()];
        args.scope = vec!["src/**".to_string()];
        let request = update_request(&args).unwrap();
        assert_eq!(request.context.as_deref(), Some("One.\nTwo."));
        assert_eq!(request.policies.unwrap().len(), 1);
        assert_eq!(request.levels, None);
        assert_eq!(request.scope.unwrap().paths, ["src/**"]);

        let mut args = update_args(&server);
        args.clear_scope = true;
        args.levels = vec!["d1:rule-1=MAY".to_string()];
        let request = update_request(&args).unwrap();
        assert!(request.scope.unwrap().paths.is_empty());
        assert_eq!(request.levels.unwrap()[0].level, RuleLevel::May);
        assert_eq!(request.title, None);
        assert_eq!(request.context, None);

        let mut args = update_args(&server);
        args.policies = vec!["no level".to_string()];
        args.levels = vec!["bad".to_string()];
        let err = update_request(&args).err();
        assert!(
            matches!(&err, Some(ActualError::AdrInvalid { reasons }) if reasons.len() == 2),
            "{err:?}"
        );
    }

    #[test]
    fn update_reads_the_decision_back() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempdir().unwrap();
        let _guards = sign_in(tmp.path());
        let mut server = mockito::Server::new();
        let update = server
            .mock("POST", format!("/v1/cli/adrs/{ADR}/update").as_str())
            .match_body(Matcher::Json(
                serde_json::json!({"org_id": ORG, "title": "New title"}),
            ))
            .with_status(200)
            .with_body(action_body("requested"))
            .expect(2)
            .create();
        let show = server
            .mock("GET", format!("/v1/cli/adrs/{ADR}").as_str())
            .match_query(Matcher::Any)
            .with_status(200)
            .with_body(format!(
                r#"{{"decision":{}}}"#,
                decision_json(ADR, "New title", "active", &[("MUST", "A.")])
            ))
            .expect(2)
            .create();
        let mut args = update_args(&server);
        args.title = Some("New title".to_string());
        let text = block(run_update(&args)).unwrap();
        assert!(text.contains("Updated"), "{text}");
        assert!(text.contains("updating the repository's rule files"));
        assert!(text.contains("New title"));
        args.api.json = true;
        let json = block(run_update(&args)).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["decision"]["title"], "New title");
        update.assert();
        show.assert();
    }

    #[test]
    fn update_says_when_the_read_back_failed() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempdir().unwrap();
        let _guards = sign_in(tmp.path());
        let mut server = mockito::Server::new();
        server
            .mock("POST", format!("/v1/cli/adrs/{ADR}/update").as_str())
            .with_status(200)
            .with_body(action_body("not_needed"))
            .create();
        server
            .mock("GET", format!("/v1/cli/adrs/{ADR}").as_str())
            .match_query(Matcher::Any)
            .with_status(500)
            .create();
        let mut args = update_args(&server);
        args.levels = vec![format!("{ADR}:rule-1=SHOULD")];
        let text = block(run_update(&args)).unwrap();
        assert!(text.contains(&format!("actual adr show {ADR}")), "{text}");
        assert!(!text.contains("updating the repository"));
    }

    #[test]
    fn update_with_a_bad_policy_sends_nothing() {
        let mut args = update_args(&mockito::Server::new());
        args.policies = vec!["nope".to_string()];
        assert!(matches!(
            block(run_update(&args)),
            Err(ActualError::AdrInvalid { .. })
        ));
    }

    // ── lifecycle and reject ──

    #[test]
    fn lifecycle_actions_say_what_they_did() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempdir().unwrap();
        let _guards = sign_in(tmp.path());
        let mut server = mockito::Server::new();
        let cases = [
            (LifecycleAction::Publish, "Published"),
            (LifecycleAction::Unpublish, "back to draft"),
            (LifecycleAction::Archive, "actual adr restore"),
            (LifecycleAction::Restore, "Restored"),
            (LifecycleAction::Accept, "Accepted"),
        ];
        for (action, expected) in cases {
            server
                .mock(
                    "POST",
                    format!("/v1/cli/adrs/{ADR}/{}", action.path_segment()).as_str(),
                )
                .match_body(Matcher::Json(serde_json::json!({"org_id": ORG})))
                .with_status(200)
                .with_body(action_body("requested"))
                .create();
            let mut args = AdrIdArgs {
                id: ADR.to_string(),
                api: api(&server),
            };
            let text = block(run_lifecycle(&args, action)).unwrap();
            assert!(text.contains(expected), "{text}");
            args.api.json = true;
            let json = block(run_lifecycle(&args, action)).unwrap();
            assert!(json.contains("\"sync\": \"requested\""), "{json}");
        }
    }

    #[test]
    fn reject_warns_when_the_reason_was_not_saved() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempdir().unwrap();
        let _guards = sign_in(tmp.path());
        let mut server = mockito::Server::new();
        server
            .mock("POST", format!("/v1/cli/adrs/{ADR}/reject").as_str())
            .match_body(Matcher::Json(
                serde_json::json!({"org_id": ORG, "reason": "We use MySQL."}),
            ))
            .with_status(200)
            .with_body(format!(
                r#"{{"success":true,"adr_id":"{ADR}","sync":"not_needed","reason_saved":false}}"#
            ))
            .create();
        let mut args = AdrRejectArgs {
            id: ADR.to_string(),
            reason: "We use MySQL.".to_string(),
            api: api(&server),
        };
        let text = block(run_reject(&args)).unwrap();
        assert!(text.contains("Rejected"), "{text}");
        assert!(text.contains("reason wasn't saved"));
        args.api.json = true;
        let json = block(run_reject(&args)).unwrap();
        assert!(json.contains("\"reason_saved\": false"), "{json}");
    }

    #[test]
    fn reject_with_a_saved_reason_prints_no_warning() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempdir().unwrap();
        let _guards = sign_in(tmp.path());
        let mut server = mockito::Server::new();
        server
            .mock("POST", format!("/v1/cli/adrs/{ADR}/reject").as_str())
            .with_status(200)
            .with_body(format!(
                r#"{{"success":true,"adr_id":"{ADR}","sync":"requested","reason_saved":true}}"#
            ))
            .create();
        let args = AdrRejectArgs {
            id: ADR.to_string(),
            reason: "r".to_string(),
            api: api(&server),
        };
        let text = block(run_reject(&args)).unwrap();
        assert!(!text.contains("warning"), "{text}");
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
            .mock("GET", "/v1/cli/adrs")
            .match_query(Matcher::Any)
            .with_status(200)
            .with_body(list_body(&[], None))
            .create();
        server
            .mock("GET", format!("/v1/cli/adrs/{ADR}").as_str())
            .match_query(Matcher::Any)
            .with_status(200)
            .with_body(format!(
                r#"{{"decision":{}}}"#,
                decision_json(ADR, "T", "active", &[])
            ))
            .create();
        server
            .mock("POST", "/v1/cli/adrs")
            .with_status(200)
            .with_body(created_body("draft"))
            .create();
        server
            .mock("POST", Matcher::Regex(format!("^/v1/cli/adrs/{ADR}/.*")))
            .with_status(200)
            .with_body(action_body("not_needed"))
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
            vec!["reject", ADR, "--reason", "r"],
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
