//! The `actual adr` commands signed out, on the decision files under
//! `docs/adr/`. A command that takes an id finds the file whose header names
//! it, the id `list` prints, and each change brings that decision's rule file
//! in step at once ([`refresh_decision`]). A file stores no policy ids, so a
//! policy goes by the rule id its rule file gives it, such as `R-0B6F-001`.
//!
//! `restore`, `accept` and `reject` act on what only Actual AI keeps (its
//! archive and its suggestions), so they need a sign-in.

use std::path::Path;

use serde::Serialize;

use super::adopt::{exec_discover, exec_normalize};
use super::cloud::{render_decision, render_list, update_edit};
use super::local::{exec_check, exec_create, render_applied};
use super::refresh::{exec_refresh, record_dir, refresh_decision};
use super::{json_text, telemetry, today};
use crate::adr::context_file::{self, js_trim, ContextFile, Policy};
use crate::adr::files::{self, split_header, write_atomic, DecisionFile, DECISION_DIR};
use crate::adr::header::{description_for, render_header, AdrHeader, REPOSITORY_ROOT};
use crate::adr::names::decision_doc_file_name;
use crate::adr::refresh::{
    fingerprint, is_decision_candidate, policy_rule_id, read_normalized, relative, Applied,
    RuleSource,
};
use crate::adr::save_check::check_document;
use crate::adr::workflow::DecisionEdit;
use crate::api::adr_types::{
    Decision, DecisionList, DecisionPolicy, DecisionScope, DecisionState, LifecycleAction,
};
use crate::cli::args::{AdrAction, AdrIdArgs, AdrListArgs, AdrShowArgs, AdrUpdateArgs};
use crate::cli::commands::sync::resolve_cwd;
use crate::cli::ui::theme;
use crate::error::{sanitize_server_text, ActualError};

pub(super) fn exec(action: &AdrAction) -> Result<(), ActualError> {
    let root = resolve_cwd();
    let record = record_dir();
    let record = record.as_deref();
    let text = match action {
        AdrAction::Create(a) => return exec_create(a),
        AdrAction::Check(a) => return exec_check(a),
        AdrAction::Discover(a) => return exec_discover(a),
        AdrAction::Normalize(a) => return exec_normalize(a),
        AdrAction::Refresh(a) => return exec_refresh(a),
        AdrAction::List(a) => run_list(a, &root),
        AdrAction::Show(a) => run_show(a, &root),
        AdrAction::Update(a) => run_update(a, &root, &today(), record),
        AdrAction::Publish(a) => run_status(a, &root, &today(), record, true),
        AdrAction::Unpublish(a) => run_status(a, &root, &today(), record, false),
        AdrAction::Delete(a) => run_delete(a, &root, &today(), record),
        AdrAction::Restore(_) => return needs_sign_in(LifecycleAction::Restore),
        AdrAction::Accept(_) => return needs_sign_in(LifecycleAction::Accept),
        AdrAction::Reject(_) => return needs_sign_in(LifecycleAction::Reject),
    }?;
    print!("{text}");
    Ok(())
}

/// Refuse a command that acts on a record only Actual AI keeps, and report
/// the run the way a signed-out run of it always was.
fn needs_sign_in(action: LifecycleAction) -> Result<(), ActualError> {
    let run = telemetry::Run::start();
    let reason = match action {
        LifecycleAction::Restore => {
            "`restore` brings a decision back from Actual AI's archive, which needs \
             `actual login`. Signed out, `delete` removes the decision's file, so git brings \
             it back: `git restore <file>`, or `git checkout <commit>^ -- <file>` once the \
             deletion is committed."
        }
        LifecycleAction::Accept => {
            "`accept` acts on a suggestion, and only Actual AI keeps suggestions. Sign in \
             with `actual login` to accept it."
        }
        _ => {
            "`reject` acts on a suggestion, and only Actual AI keeps suggestions. Sign in \
             with `actual login` to reject it."
        }
    };
    let result: Result<(), ActualError> = Err(ActualError::AdrPathway(reason.to_string()));
    telemetry::report_review(&run, telemetry::command(action), None, &result);
    result
}

// ── reading ────────────────────────────────────────────────────────────────

/// The decision file under `docs/adr/` whose header names `id`.
fn find(root: &Path, id: &str) -> Result<DecisionFile, ActualError> {
    files::discover(root)
        .decisions
        .into_iter()
        .find(|file| file.adr_id() == Some(id))
        .ok_or_else(|| {
            ActualError::AdrNotFound(format!(
                "No decision file under {DECISION_DIR}/ has the id {}.",
                sanitize_server_text(id)
            ))
        })
}

/// A decision file as `list` and `show` print a decision: its status as the
/// header has it, and `active` for one that governs.
fn view(file: &DecisionFile) -> Decision {
    let id = file.adr_id().unwrap_or_default();
    let meta = file.meta.clone().unwrap_or_default();
    let doc = context_file::parse(&file.body)
        .map(|parsed| parsed.doc)
        .unwrap_or_default();
    let status = meta
        .status
        .clone()
        .unwrap_or_else(|| "accepted".to_string());
    let state = if meta.is_active() {
        DecisionState::Active.as_str().to_string()
    } else {
        status.trim().to_lowercase()
    };
    let title = Some(js_trim(&doc.title).to_string())
        .filter(|title| !title.is_empty())
        .or(meta.description.clone())
        .unwrap_or_default();
    Decision {
        id: id.to_string(),
        kind: "single".to_string(),
        repo_unique_id: None,
        title,
        state,
        status,
        date: doc.date,
        context: (!doc.context.is_empty()).then(|| doc.context.join("\n")),
        policies: doc
            .policies
            .iter()
            .enumerate()
            .map(|(i, policy)| DecisionPolicy {
                id: policy_rule_id(id, i + 1),
                level: policy.level,
                statement: policy.statement.clone(),
            })
            .collect(),
        scope: Some(DecisionScope {
            root: meta.root.unwrap_or_else(|| REPOSITORY_ROOT.to_string()),
            paths: meta.paths,
        }),
        category: None,
        base_adr_id: None,
    }
}

/// A decision with the file it came from, for `--json`.
#[derive(Debug, Serialize)]
struct FileDecision {
    #[serde(flatten)]
    decision: Decision,
    file: String,
}

/// A page of `list`, for `--json`.
#[derive(Debug, Serialize)]
struct FileDecisionList {
    decisions: Vec<FileDecision>,
    total: u64,
    next_offset: Option<u64>,
}

fn run_list(args: &AdrListArgs, root: &Path) -> Result<String, ActualError> {
    if args.repo.is_some() {
        return Err(ActualError::AdrPathway(format!(
            "`--repo` picks a connected repository in Actual AI, which needs `actual login`. \
             Signed out, `list` reads the files under {DECISION_DIR}/ in this checkout."
        )));
    }
    if let Some(state) = args
        .state
        .filter(|state| !matches!(state, DecisionState::Active | DecisionState::Draft))
    {
        return Err(ActualError::AdrPathway(format!(
            "Only Actual AI keeps {} decisions, so `--state {}` needs `actual login`. Signed \
             out, a decision file is active or a draft.",
            state.as_str(),
            state.as_str()
        )));
    }
    let found = files::discover(root);
    let matching: Vec<FileDecision> = found
        .decisions
        .iter()
        .map(|file| FileDecision {
            decision: view(file),
            file: relative(root, &file.path),
        })
        .filter(|listed| {
            args.state
                .is_none_or(|state| listed.decision.state == state.as_str())
        })
        .collect();
    let total = matching.len() as u64;
    let shown: Vec<FileDecision> = matching
        .into_iter()
        .skip(usize::try_from(args.offset).unwrap_or(usize::MAX))
        .take(args.limit as usize)
        .collect();
    let end = args.offset.saturating_add(shown.len() as u64);
    let next_offset = (end < total).then_some(end);
    if args.api.json {
        let page = FileDecisionList {
            decisions: shown,
            total,
            next_offset,
        };
        return Ok(format!("{}\n", json_text(&page)));
    }
    let page = DecisionList {
        decisions: shown.into_iter().map(|listed| listed.decision).collect(),
        total,
        next_offset,
    };
    let mut out = render_list(&page, &format!("{DECISION_DIR}/"), args.offset);
    let unheaded = found
        .skipped
        .iter()
        .filter(|path| is_decision_candidate(Path::new(&relative(root, path))))
        .count();
    if unheaded > 0 {
        out.push_str(&format!(
            "\n{unheaded} {} under {DECISION_DIR}/ {} no ADR header, so `list` leaves {} out; \
             `actual adr check` names {}.\n",
            if unheaded == 1 { "file" } else { "files" },
            if unheaded == 1 { "has" } else { "have" },
            if unheaded == 1 { "it" } else { "them" },
            if unheaded == 1 { "it" } else { "them" },
        ));
    }
    Ok(out)
}

fn run_show(args: &AdrShowArgs, root: &Path) -> Result<String, ActualError> {
    let file = find(root, &args.id)?;
    let shown = FileDecision {
        decision: view(&file),
        file: relative(root, &file.path),
    };
    Ok(if args.api.json {
        format!("{}\n", json_text(&shown))
    } else {
        render_decision(&shown.decision, Some(&shown.file))
    })
}

// ── changes ────────────────────────────────────────────────────────────────

/// What a signed-out change did, for `--json`.
#[derive(Debug, Serialize)]
struct Changed {
    success: bool,
    adr_id: String,
    /// The decision file, as it is after the change; for `delete`, the file
    /// removed.
    file: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    status: Option<String>,
    rule_files: Applied,
}

/// What the rule file of `file`'s decision is written from, while the file
/// reads as a decision.
fn rule_source(file: &DecisionFile, doc: &ContextFile) -> RuleSource {
    let meta = file.meta.clone().unwrap_or_default();
    RuleSource {
        adr_id: file.adr_id().unwrap_or_default().to_string(),
        title: js_trim(&doc.title).to_string(),
        status: meta.status.unwrap_or_else(|| "accepted".to_string()),
        scope_root: meta.root.unwrap_or_else(|| REPOSITORY_ROOT.to_string()),
        scope_paths: meta.paths,
        policies: doc.policies.clone(),
    }
}

/// The header `file` carries, rebuilt from the fields it read.
fn header_of(file: &DecisionFile) -> AdrHeader {
    let meta = file.meta.clone().unwrap_or_default();
    let stem = file
        .path
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default();
    AdrHeader {
        name: meta.name.unwrap_or(stem),
        description: meta.description.unwrap_or_default(),
        adr_id: meta.adr_id.unwrap_or_default(),
        status: meta.status.unwrap_or_default(),
        scope_root: meta.root.unwrap_or_else(|| REPOSITORY_ROOT.to_string()),
        scope_paths: meta.paths,
        source: None,
    }
}

/// The value of the `Status:` line before a Context File's first section.
fn status_line(body: &str) -> Option<&str> {
    body.lines()
        .take_while(|line| !line.starts_with("## "))
        .find_map(|line| line.strip_prefix("Status:"))
        .map(str::trim)
}

/// `file`'s decision when the file is laid out the way `create` and the
/// sync write one, so `update` can write it back whole without dropping
/// anything; else why not.
fn rewritable(file: &DecisionFile, shown: &str) -> Result<ContextFile, ActualError> {
    let not_rewritable = |what: &str| {
        ActualError::AdrNotAllowed(format!(
            "{shown} has {what} outside the layout `actual adr create` writes, which `update` \
             would drop. Edit the file itself; the next `actual adr refresh` brings its rule \
             file in step."
        ))
    };
    let text = std::fs::read_to_string(&file.path)?;
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    let header = split_header(text).map_or("", |(header, _)| header);
    if render_header(&header_of(file)) != header {
        return Err(not_rewritable("header fields"));
    }
    let mut doc = context_file::parse(&file.body)
        .map_err(|reason| ActualError::AdrInvalid {
            reasons: vec![reason],
        })?
        .doc;
    doc.status = status_line(&file.body).unwrap_or_default().to_string();
    if context_file::render(&doc) != js_trim(&file.body) {
        return Err(not_rewritable("text"));
    }
    Ok(doc)
}

/// `doc` with the edit applied. A policy id has to be one of the decision's.
fn edited(
    doc: &ContextFile,
    edit: &DecisionEdit,
    adr_id: &str,
) -> Result<ContextFile, ActualError> {
    let ids: Vec<String> = (1..=doc.policies.len())
        .map(|n| policy_rule_id(adr_id, n))
        .collect();
    let position = |id: &str| {
        ids.iter()
            .position(|known| known.eq_ignore_ascii_case(id.trim()))
            .ok_or_else(|| {
                ActualError::AdrNotFound(format!(
                    "Decision {} has no policy {}; `actual adr show {}` lists its policy ids.",
                    sanitize_server_text(adr_id),
                    sanitize_server_text(id),
                    sanitize_server_text(adr_id)
                ))
            })
    };
    let mut doc = doc.clone();
    if let Some(title) = &edit.title {
        doc.title = title.clone();
    }
    if let Some(context) = &edit.context {
        doc.context = context
            .lines()
            .map(str::trim)
            .filter(|point| !point.is_empty())
            .map(str::to_string)
            .collect();
    }
    if let Some(policies) = &edit.policies {
        for kept in policies.iter().filter_map(|policy| policy.id.as_deref()) {
            position(kept)?;
        }
        doc.policies = policies
            .iter()
            .map(|policy| Policy {
                level: policy.level,
                statement: policy.statement.clone(),
            })
            .collect();
    }
    for level in &edit.levels {
        let n = position(&level.id)?;
        doc.policies[n].level = level.level;
    }
    Ok(doc)
}

fn run_update(
    args: &AdrUpdateArgs,
    root: &Path,
    today: &str,
    record: Option<&Path>,
) -> Result<String, ActualError> {
    let edit = update_edit(args)?;
    let file = find(root, &args.id)?;
    let adr_id = file.adr_id().unwrap_or_default().to_string();
    let shown = relative(root, &file.path);
    let doc = rewritable(&file, &shown)?;
    let before = fingerprint(&rule_source(&file, &doc));
    let changed = edited(&doc, &edit, &adr_id)?;
    // The save check normalizes the text and stamps today's date and its own
    // status; the file keeps its own date and Status line.
    let checked = check_document(&changed, file.is_draft(), today)
        .map_err(|reasons| ActualError::AdrInvalid { reasons })?;
    let changed = ContextFile {
        date: doc.date.clone(),
        status: doc.status.clone(),
        ..checked
    };

    let mut header = header_of(&file);
    if let Some(paths) = &edit.scope {
        header.scope_paths = paths.clone();
    }
    let mut path = file.path.clone();
    if js_trim(&changed.title) != js_trim(&doc.title) {
        header.description = description_for(&changed.title);
        // A file under the sync's name moves to the name for the new title.
        let file_name = path.file_name().and_then(|name| name.to_str());
        if file_name == Some(decision_doc_file_name(&adr_id, &doc.title).as_str()) {
            let renamed = decision_doc_file_name(&adr_id, &changed.title);
            if !path.with_file_name(&renamed).exists() {
                header.name = renamed.trim_end_matches(".md").to_string();
                path = path.with_file_name(renamed);
            }
        }
    }
    write_atomic(
        &path,
        &format!(
            "{}\n{}",
            render_header(&header),
            context_file::render(&changed)
        ),
    )?;
    if path != file.path {
        std::fs::remove_file(&file.path)?;
    }
    let file = files::read_decision_file(&path)?;
    let after = fingerprint(&rule_source(&file, &changed));
    let applied = refresh_decision(root, &adr_id, today, record, before != after);
    let shown = relative(root, &path);
    if args.api.json {
        return Ok(format!(
            "{}\n",
            json_text(&Changed {
                success: true,
                adr_id,
                file: shown,
                status: None,
                rule_files: applied,
            })
        ));
    }
    Ok(format!(
        "{} Updated {} in {shown}.\n\n{}{}",
        theme::success(&theme::SUCCESS),
        sanitize_server_text(&adr_id),
        render_decision(&view(&file), Some(&shown)),
        render_applied(&applied)
    ))
}

/// `text` with its header's status set to `status` and its Context File's
/// `Status:` line, when it has one, set to `line`.
fn with_status(text: &str, status: &str, line: &str) -> Option<String> {
    let (header, body) = split_header(text)?;
    let mut found = false;
    let header: String = header
        .split_inclusive('\n')
        .map(|row| {
            if !found && row.starts_with("    status:") {
                found = true;
                format!("    status: {status}\n")
            } else {
                row.to_string()
            }
        })
        .collect();
    if !found {
        return None;
    }
    let mut in_preamble = true;
    let body: String = body
        .split_inclusive('\n')
        .map(|row| {
            if row.starts_with("## ") {
                in_preamble = false;
            }
            if in_preamble && row.starts_with("Status:") {
                in_preamble = false;
                let end = if row.ends_with('\n') { "\n" } else { "" };
                format!("Status: {line}{end}")
            } else {
                row.to_string()
            }
        })
        .collect();
    Some(format!("{header}\n{body}"))
}

/// `publish` (accepted) or `unpublish` (draft) a decision file.
fn run_status(
    args: &AdrIdArgs,
    root: &Path,
    today: &str,
    record: Option<&Path>,
    publish: bool,
) -> Result<String, ActualError> {
    let file = find(root, &args.id)?;
    let adr_id = file.adr_id().unwrap_or_default().to_string();
    let id = sanitize_server_text(&adr_id);
    let meta = file.meta.clone().unwrap_or_default();
    let was = meta
        .status
        .clone()
        .unwrap_or_else(|| "accepted".to_string());
    let (status, line) = if publish {
        if meta.is_active() {
            return Err(ActualError::AdrNotAllowed(format!(
                "Decision {id} is already {}, so there's nothing to publish.",
                sanitize_server_text(&was)
            )));
        }
        // An accepted file has to read as a decision, or it gets no rule file.
        read_normalized(&file, false, today)
            .map_err(|reasons| ActualError::AdrInvalid { reasons })?;
        ("accepted", "accepted")
    } else {
        if !meta.is_active() {
            return Err(ActualError::AdrNotAllowed(format!(
                "Decision {id} is {}, not accepted, so there's nothing to unpublish.",
                sanitize_server_text(&was)
            )));
        }
        // The Status line a draft's Context File carries, as the save writes it.
        ("draft", "proposed")
    };
    let text = std::fs::read_to_string(&file.path)?;
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    let rewritten = with_status(text, status, line).ok_or_else(|| {
        ActualError::AdrNotAllowed(format!(
            "{}'s header has no status line to change.",
            relative(root, &file.path)
        ))
    })?;
    write_atomic(&file.path, &rewritten)?;
    let applied = refresh_decision(root, &adr_id, today, record, true);
    let shown = relative(root, &file.path);
    if args.api.json {
        return Ok(format!(
            "{}\n",
            json_text(&Changed {
                success: true,
                adr_id,
                file: shown,
                status: Some(status.to_string()),
                rule_files: applied,
            })
        ));
    }
    let (verb, now) = if publish {
        (
            "Published",
            "is accepted, so plan-check and impl-check enforce it now",
        )
    } else {
        ("Unpublished", "is a draft, which no check reads")
    };
    Ok(format!(
        "{} {verb} {id}: {shown} {now}.\n{}",
        theme::success(&theme::SUCCESS),
        render_applied(&applied)
    ))
}

fn run_delete(
    args: &AdrIdArgs,
    root: &Path,
    today: &str,
    record: Option<&Path>,
) -> Result<String, ActualError> {
    let file = find(root, &args.id)?;
    let adr_id = file.adr_id().unwrap_or_default().to_string();
    std::fs::remove_file(&file.path)?;
    let applied = refresh_decision(root, &adr_id, today, record, false);
    let shown = relative(root, &file.path);
    if args.api.json {
        return Ok(format!(
            "{}\n",
            json_text(&Changed {
                success: true,
                adr_id,
                file: shown,
                status: None,
                rule_files: applied,
            })
        ));
    }
    Ok(format!(
        "{} Deleted {}: removed {shown}.\n{}`git restore {shown}` brings it back.\n",
        theme::success(&theme::SUCCESS),
        sanitize_server_text(&adr_id),
        render_applied(&applied)
    ))
}

#[cfg(test)]
mod tests {
    use super::super::local::write_new;
    use super::*;
    use crate::adr::files::rule_files_by_id;
    use crate::adr::names::rule_file_name;
    use crate::api::adr_types::SaveStatus;
    use crate::cli::args::{AdrApiOptions, AdrContentArgs, AdrCreateArgs};
    use crate::rules::types::RuleLevel;
    use tempfile::tempdir;

    const TODAY: &str = "2026-11-20";

    /// Write a decision with `create`, as a person signed out would.
    fn create(root: &Path, title: &str, policies: &[&str], status: SaveStatus) -> String {
        let args = AdrCreateArgs {
            title: Some(title.to_string()),
            content: AdrContentArgs {
                context: vec!["We need one relational store.".to_string()],
                policies: policies.iter().map(|p| p.to_string()).collect(),
            },
            status: Some(status),
            ..AdrCreateArgs::default()
        };
        write_new(&args, root, TODAY, None).unwrap().adr_id
    }

    fn list_args() -> AdrListArgs {
        AdrListArgs {
            repo: None,
            state: None,
            limit: 50,
            offset: 0,
            api: AdrApiOptions::default(),
        }
    }

    fn id_args(id: &str, json: bool) -> AdrIdArgs {
        AdrIdArgs {
            id: id.to_string(),
            api: AdrApiOptions {
                api_url: None,
                json,
            },
        }
    }

    fn update_args(id: &str) -> AdrUpdateArgs {
        AdrUpdateArgs {
            id: id.to_string(),
            title: None,
            context: Vec::new(),
            policies: Vec::new(),
            levels: Vec::new(),
            scope: Vec::new(),
            clear_scope: false,
            api: AdrApiOptions::default(),
        }
    }

    fn rule_text(root: &Path, id: &str) -> Option<String> {
        rule_files_by_id(root)
            .get(id)
            .map(|path| std::fs::read_to_string(path).unwrap())
    }

    #[test]
    fn list_reads_each_decision_file_with_its_state_and_pages() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        let accepted = create(
            root,
            "Use Postgres",
            &["MUST: Use Postgres."],
            SaveStatus::Accepted,
        );
        let draft = create(
            root,
            "Use Redis",
            &["SHOULD: Use Redis."],
            SaveStatus::Draft,
        );
        std::fs::write(root.join("docs/adr/plain.md"), "# Use Kafka\n").unwrap();
        std::fs::write(root.join("docs/adr/README.md"), "# Decisions\n").unwrap();

        let text = run_list(&list_args(), root).unwrap();
        assert!(text.contains("Decisions in docs/adr/: 1-2 of 2"), "{text}");
        assert!(
            text.contains(&format!("{accepted} · active · 1 policy")),
            "{text}"
        );
        assert!(
            text.contains(&format!("{draft} · draft · 1 policy")),
            "{text}"
        );
        assert!(
            text.contains("1 file under docs/adr/ has no ADR header, so `list` leaves it out"),
            "{text}"
        );

        let mut drafts = list_args();
        drafts.state = Some(DecisionState::Draft);
        drafts.api.json = true;
        let json: serde_json::Value =
            serde_json::from_str(&run_list(&drafts, root).unwrap()).unwrap();
        assert_eq!(json["total"], 1);
        assert_eq!(json["decisions"][0]["id"], draft.as_str());
        assert_eq!(json["decisions"][0]["status"], "draft");
        assert!(json["decisions"][0]["file"]
            .as_str()
            .unwrap()
            .starts_with("docs/adr/"));

        let mut first = list_args();
        first.limit = 1;
        let text = run_list(&first, root).unwrap();
        assert!(
            text.contains("1-1 of 2") && text.contains("--offset 1"),
            "{text}"
        );
    }

    #[test]
    fn list_refuses_what_only_actual_ai_keeps() {
        let tmp = tempdir().unwrap();
        let mut by_repo = list_args();
        by_repo.repo = Some("web".to_string());
        let mut archived = list_args();
        archived.state = Some(DecisionState::Archived);
        for args in [by_repo, archived] {
            let err = run_list(&args, tmp.path()).unwrap_err();
            assert!(
                matches!(&err, ActualError::AdrPathway(m) if m.contains("actual login")),
                "{err:?}"
            );
        }
        assert_eq!(
            run_list(&list_args(), tmp.path()).unwrap(),
            "No decisions in docs/adr/.\n"
        );
    }

    #[test]
    fn show_finds_a_decision_file_by_its_id_with_rule_ids_for_policies() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        let id = create(
            root,
            "Use Postgres",
            &["MUST: Use Postgres.", "MUST NOT: Add a second database."],
            SaveStatus::Accepted,
        );
        let show = AdrShowArgs {
            id: id.clone(),
            api: AdrApiOptions::default(),
        };
        let text = run_show(&show, root).unwrap();
        assert!(text.contains("File:"), "{text}");
        assert!(text.contains("active (accepted)"), "{text}");
        assert!(text.contains(&policy_rule_id(&id, 2)), "{text}");
        assert!(text.contains("We need one relational store."), "{text}");

        let json: serde_json::Value = serde_json::from_str(
            &run_show(
                &AdrShowArgs {
                    api: AdrApiOptions {
                        api_url: None,
                        json: true,
                    },
                    ..show
                },
                root,
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(json["policies"][1]["id"], policy_rule_id(&id, 2));
        assert_eq!(json["policies"][1]["level"], "MUST_NOT");
        assert_eq!(json["scope"]["root"], ".");

        let missing = AdrShowArgs {
            id: "nope".to_string(),
            api: AdrApiOptions::default(),
        };
        let err = run_show(&missing, root).unwrap_err();
        assert!(matches!(err, ActualError::AdrNotFound(_)), "{err:?}");
        assert_eq!(err.exit_code(), 3);
    }

    #[test]
    fn update_rewrites_the_file_and_its_rule_file() {
        let tmp = tempdir().unwrap();
        let records = tempdir().unwrap();
        let root = tmp.path();
        let id = create(
            root,
            "Use Postgres",
            &["MUST: Use Postgres.", "MAY: Use SQLite in tests."],
            SaveStatus::Accepted,
        );
        let old = root
            .join(DECISION_DIR)
            .join(decision_doc_file_name(&id, "Use Postgres"));

        let mut args = update_args(&id);
        args.title = Some("Use Postgres for storage".to_string());
        args.levels = vec![format!("{}=SHOULD", policy_rule_id(&id, 2).to_lowercase())];
        args.scope = vec!["services/**".to_string()];
        let text = run_update(&args, root, TODAY, Some(records.path())).unwrap();
        assert!(text.contains("Updated"), "{text}");
        assert!(text.contains("Rewrote rule file"), "{text}");
        let renamed = root
            .join(DECISION_DIR)
            .join(decision_doc_file_name(&id, "Use Postgres for storage"));
        assert!(!old.exists() && renamed.exists());
        let file = files::read_decision_file(&renamed).unwrap();
        assert_eq!(file.meta.as_ref().unwrap().paths, ["services/**"]);
        let doc = context_file::parse(&file.body).unwrap().doc;
        assert_eq!(doc.title, "Use Postgres for storage");
        assert_eq!(doc.policies[1].level, RuleLevel::Should);
        // The file keeps its own date and Status line.
        assert!(file.body.contains("Status: accepted"), "{}", file.body);
        let rule = rule_text(root, &id).unwrap();
        assert!(rule.contains("SHOULD: Use SQLite in tests."), "{rule}");
        assert!(rule.contains("`services/**`"), "{rule}");

        // The whole policy list, one kept by id and one new; context cleared.
        let mut args = update_args(&id);
        args.policies = vec![
            format!("{}=MUST: Use Postgres 16.", policy_rule_id(&id, 1)),
            "MUST NOT: Add a second database.".to_string(),
        ];
        args.context = vec![String::new()];
        args.api.json = true;
        let json: serde_json::Value =
            serde_json::from_str(&run_update(&args, root, TODAY, Some(records.path())).unwrap())
                .unwrap();
        assert_eq!(json["adr_id"], id.as_str());
        assert_eq!(json["rule_files"]["rewritten"].as_array().unwrap().len(), 1);
        let file = files::read_decision_file(&renamed).unwrap();
        let doc = context_file::parse(&file.body).unwrap().doc;
        assert!(doc.context.is_empty());
        assert_eq!(doc.policies.len(), 2);
        assert_eq!(doc.policies[1].statement, "Add a second database.");

        // Only the context changes: the rule file says the same, and stays.
        let mut args = update_args(&id);
        args.context = vec!["Three engines run in production.".to_string()];
        let text = run_update(&args, root, TODAY, Some(records.path())).unwrap();
        assert!(!text.contains("rule file"), "{text}");
    }

    #[test]
    fn update_refuses_an_unknown_policy_a_blank_title_and_a_file_it_would_cut() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        let id = create(
            root,
            "Use Postgres",
            &["MUST: Use Postgres."],
            SaveStatus::Accepted,
        );

        let mut unknown = update_args(&id);
        unknown.levels = vec!["R-FFFF-009=MAY".to_string()];
        let err = run_update(&unknown, root, TODAY, None).unwrap_err();
        assert!(
            matches!(&err, ActualError::AdrNotFound(m) if m.contains("has no policy R-FFFF-009")),
            "{err:?}"
        );

        let mut blank = update_args(&id);
        blank.title = Some("  ".to_string());
        assert!(matches!(
            run_update(&blank, root, TODAY, None),
            Err(ActualError::AdrInvalid { .. })
        ));

        // A section of its own, which a rewrite would drop.
        let path = root
            .join(DECISION_DIR)
            .join(decision_doc_file_name(&id, "Use Postgres"));
        let text = format!(
            "{}\n\n## Notes\n\n- Ours.\n",
            std::fs::read_to_string(&path).unwrap()
        );
        std::fs::write(&path, &text).unwrap();
        let mut title = update_args(&id);
        title.title = Some("Use Postgres 16".to_string());
        let err = run_update(&title, root, TODAY, None).unwrap_err();
        assert!(
            matches!(&err, ActualError::AdrNotAllowed(m) if m.contains("which `update` would drop")),
            "{err:?}"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
    }

    #[test]
    fn publish_and_unpublish_move_the_status_and_the_rule_file() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        let id = create(
            root,
            "Use Redis",
            &["SHOULD: Use Redis."],
            SaveStatus::Draft,
        );
        assert_eq!(rule_text(root, &id), None);

        let text = run_status(&id_args(&id, false), root, TODAY, None, true).unwrap();
        assert!(
            text.contains("Published") && text.contains("Wrote rule file"),
            "{text}"
        );
        let file = find(root, &id).unwrap();
        assert_eq!(
            file.meta.as_ref().unwrap().status.as_deref(),
            Some("accepted")
        );
        assert!(file.body.contains("Status: accepted"), "{}", file.body);
        assert!(rule_text(root, &id).unwrap().contains("SHOULD: Use Redis."));
        let err = run_status(&id_args(&id, false), root, TODAY, None, true).unwrap_err();
        assert!(
            matches!(&err, ActualError::AdrNotAllowed(m) if m.contains("already accepted")),
            "{err:?}"
        );

        let json: serde_json::Value = serde_json::from_str(
            &run_status(&id_args(&id, true), root, TODAY, None, false).unwrap(),
        )
        .unwrap();
        assert_eq!(json["status"], "draft");
        assert_eq!(json["rule_files"]["removed"].as_array().unwrap().len(), 1);
        let file = find(root, &id).unwrap();
        assert!(file.is_draft());
        assert!(file.body.contains("Status: proposed"), "{}", file.body);
        assert_eq!(rule_text(root, &id), None);
        assert!(matches!(
            run_status(&id_args(&id, false), root, TODAY, None, false),
            Err(ActualError::AdrNotAllowed(_))
        ));
    }

    #[test]
    fn publish_refuses_a_draft_that_would_get_no_rule_file() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        let id = create(
            root,
            "Use Redis",
            &["SHOULD: Use Redis."],
            SaveStatus::Draft,
        );
        let path = find(root, &id).unwrap().path;
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::write(&path, text.replace("version: 2\n", "")).unwrap();
        let err = run_status(&id_args(&id, false), root, TODAY, None, true).unwrap_err();
        assert!(
            matches!(&err, ActualError::AdrInvalid { reasons } if reasons[0].contains("version: 2")),
            "{err:?}"
        );
        assert!(find(root, &id).unwrap().is_draft());
    }

    #[test]
    fn delete_removes_the_file_and_its_rule_file() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        let id = create(
            root,
            "Use Postgres",
            &["MUST: Use Postgres."],
            SaveStatus::Accepted,
        );
        let rule = root
            .join(".actual/rules")
            .join(rule_file_name(&id, "Use Postgres"));
        assert!(rule.exists());
        let text = run_delete(&id_args(&id, false), root, TODAY, None).unwrap();
        assert!(
            text.contains("Deleted") && text.contains("git restore"),
            "{text}"
        );
        assert!(text.contains("Removed rule file"), "{text}");
        assert!(!rule.exists());
        assert!(matches!(find(root, &id), Err(ActualError::AdrNotFound(_))));
        assert!(matches!(
            run_delete(&id_args(&id, true), root, TODAY, None),
            Err(ActualError::AdrNotFound(_))
        ));
    }

    #[test]
    fn restore_accept_and_reject_need_a_sign_in() {
        for (action, word) in [
            (LifecycleAction::Restore, "git restore"),
            (LifecycleAction::Accept, "`accept` acts on a suggestion"),
            (LifecycleAction::Reject, "`reject` acts on a suggestion"),
        ] {
            let err = needs_sign_in(action).unwrap_err();
            assert!(
                matches!(&err, ActualError::AdrPathway(m) if m.contains(word)),
                "{err:?}"
            );
            assert_eq!(err.exit_code(), 2);
        }
    }

    #[test]
    fn with_status_changes_the_header_and_the_status_line_only() {
        let text = "---\nversion: 2\nmetadata:\n  actual:\n    adr_id: a\n    status: draft\n---\n\n# T\n\nStatus: proposed\nDate: 2026-11-20\n\n## Context\n\nStatus: kept\n";
        let published = with_status(text, "accepted", "accepted").unwrap();
        assert!(published.contains("    status: accepted\n"));
        assert!(published.contains("\nStatus: accepted\nDate"));
        assert!(published.ends_with("Status: kept\n"), "{published}");
        assert_eq!(with_status("# No header\n", "accepted", "accepted"), None);
        assert_eq!(
            with_status("---\nversion: 2\n---\n\n# T\n", "accepted", "accepted"),
            None
        );
        assert_eq!(status_line("# T\n\nStatus:  draft \n"), Some("draft"));
        assert_eq!(status_line("# T\n\n## Context\n\nStatus: x\n"), None);
    }
}
