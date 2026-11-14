//! `actual adr new` and `actual adr check`: decisions as files in this
//! repository, with no sign-in needed.

use std::io::Write;
use std::path::{Path, PathBuf};

use serde::Serialize;
use uuid::Uuid;

use super::{
    content_from_flags, interactive, json_text, pick_repo, rule_spelling, runtime, save_status,
    session, today, RepoPick,
};
use crate::adr::context_file::{self, ContextFile};
use crate::adr::files::{self, DecisionFile, DECISION_DIR, RULES_DIR};
use crate::adr::header::{description_for, render_header, AdrHeader};
use crate::adr::names::{decision_doc_file_name, rule_file_name};
use crate::adr::overlap::{find_overlaps, ExistingDecision, Overlap};
use crate::adr::rule_body::{render_rule_body, rule_id_prefix, scope_line};
use crate::adr::save_check::check_document;
use crate::adr::save_check::check_markdown;
use crate::api::adr_types::SaveStatus;
use crate::cli::args::{AdrCheckArgs, AdrNewArgs};
use crate::cli::commands::sync::resolve_cwd;
use crate::cli::ui::theme;
use crate::error::{sanitize_server_text, ActualError};

// ── new ────────────────────────────────────────────────────────────────────

/// What `adr new` wrote.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(super) struct Written {
    pub(super) adr_id: String,
    pub(super) status: SaveStatus,
    pub(super) decision_file: PathBuf,
    pub(super) rule_file: PathBuf,
}

pub(super) fn exec_new(args: &AdrNewArgs) -> Result<(), ActualError> {
    let root = args.root.clone().unwrap_or_else(resolve_cwd);
    let written = write_new(args, &root, interactive(), &today())?;
    if args.json {
        println!("{}", json_text(&written));
    } else {
        print!("{}", render_written(&written));
    }
    Ok(())
}

/// Check the decision the flags describe, then write its decision doc and
/// rule file under a new local id, as the repository sync lays them out.
pub(super) fn write_new(
    args: &AdrNewArgs,
    root: &Path,
    interactive: bool,
    today: &str,
) -> Result<Written, ActualError> {
    let status = save_status(args.status, interactive);
    let doc = ContextFile {
        deciders: args.deciders.clone().unwrap_or_default(),
        ..content_from_flags(&args.title, &args.content)?
    };
    let doc = check_document(&doc, status == SaveStatus::Draft, today)
        .map_err(|reasons| ActualError::AdrInvalid { reasons })?;
    let adr_id = Uuid::new_v4().to_string();
    let [(doc_name, doc_text), (rule_name, rule_text)] =
        render_pair(&doc, &adr_id, status, &args.scope);
    let decision_file = root.join(DECISION_DIR).join(doc_name);
    let rule_file = root.join(RULES_DIR).join(rule_name);
    write_new_file(&decision_file, &doc_text)?;
    if let Err(e) = write_new_file(&rule_file, &rule_text) {
        // Never leave half a pair behind.
        let _ = std::fs::remove_file(&decision_file);
        return Err(e);
    }
    Ok(Written {
        adr_id,
        status,
        decision_file,
        rule_file,
    })
}

/// The decision doc and the rule file for `doc` under `adr_id`, each as a
/// file name and its text.
fn render_pair(
    doc: &ContextFile,
    adr_id: &str,
    status: SaveStatus,
    scope: &[String],
) -> [(String, String); 2] {
    let header = |file_name: &str| {
        render_header(&AdrHeader {
            name: file_name.trim_end_matches(".md").to_string(),
            description: description_for(&doc.title),
            adr_id: adr_id.to_string(),
            status: status.as_str().to_string(),
            scope_root: ".".to_string(),
            scope_paths: scope.to_vec(),
        })
    };
    let doc_name = decision_doc_file_name(adr_id, &doc.title);
    let doc_text = format!("{}\n{}", header(&doc_name), context_file::render(doc));
    let rules: Vec<(&str, &str)> = doc
        .policies
        .iter()
        .map(|policy| (rule_spelling(policy.level), policy.statement.as_str()))
        .collect();
    let body = render_rule_body(
        &doc.title,
        &scope_line(".", scope),
        &rule_id_prefix(adr_id),
        &rules,
    );
    let rule_name = rule_file_name(adr_id, &doc.title);
    let rule_text = format!("{}\n{body}", header(&rule_name));
    [(doc_name, doc_text), (rule_name, rule_text)]
}

/// Write `text` to a new file at `path`, creating its directory; an
/// existing file is never replaced.
fn write_new_file(path: &Path, text: &str) -> Result<(), ActualError> {
    path.parent().map(std::fs::create_dir_all).transpose()?;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    file.write_all(text.as_bytes())?;
    Ok(())
}

fn render_written(written: &Written) -> String {
    let governs = match written.status {
        SaveStatus::Accepted => "accepted, so plan-check and impl-check enforce it now",
        SaveStatus::Draft => "a draft, which no check reads until its status is accepted",
    };
    format!(
        "{} Wrote decision {}\n  {}\n  {}\nIt is {governs}.\nNext: `actual adr check`, then \
         `actual adr create --file {}` to save it to Actual AI.\n",
        theme::success(&theme::SUCCESS),
        written.adr_id,
        written.decision_file.display(),
        written.rule_file.display(),
        written.decision_file.display(),
    )
}

// ── check ──────────────────────────────────────────────────────────────────

/// One file's result.
#[derive(Debug, Clone, PartialEq, Serialize)]
struct FileResult {
    path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    adr_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    title: Option<String>,
    ok: bool,
    problems: Vec<String>,
    warnings: Vec<String>,
}

/// A likely duplicate of one checked file: another file in the repository
/// (`other_file`), or a decision in Actual AI (`other_decision`).
#[derive(Debug, Clone, PartialEq, Serialize)]
struct Duplicate {
    file: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    other_file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    other_decision: Option<String>,
    overlap: Overlap,
}

/// Whether the cloud comparison ran, against what, and why not.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
struct CloudComparison {
    compared: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    scope: Option<String>,
    decisions: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct CheckReport {
    files: Vec<FileResult>,
    duplicates: Vec<Duplicate>,
    /// Markdown under `docs/adr/` with no ADR header, left unchecked.
    skipped: Vec<String>,
    cloud: CloudComparison,
}

impl CheckReport {
    fn failed(&self) -> usize {
        self.files.iter().filter(|file| !file.ok).count()
    }
}

pub(super) fn exec_check(args: &AdrCheckArgs) -> Result<(), ActualError> {
    let root = args.root.clone().unwrap_or_else(resolve_cwd);
    let report = runtime()?.block_on(run_check(args, &root, &today()));
    if args.api.json {
        println!("{}", json_text(&report));
    } else {
        print!("{}", render_report(&report));
    }
    match report.failed() {
        0 => Ok(()),
        failed => Err(ActualError::AdrCheckFailed(format!(
            "{failed} of {} decision files failed the check",
            report.files.len()
        ))),
    }
}

/// A file checked, with the decision it describes when it parses.
struct Checked {
    result: FileResult,
    doc: Option<ContextFile>,
}

async fn run_check(args: &AdrCheckArgs, root: &Path, today: &str) -> CheckReport {
    let (files, unreadable, skipped) = if args.files.is_empty() {
        let found = files::discover(root);
        (found.decisions, found.unreadable, found.skipped)
    } else {
        let mut files = Vec::new();
        let mut unreadable = Vec::new();
        for path in &args.files {
            match files::read_decision_file(path) {
                Ok(file) => files.push(file),
                Err(e) => unreadable.push((path.clone(), e.to_string())),
            }
        }
        (files, unreadable, Vec::new())
    };

    let mut checked: Vec<Checked> = files
        .iter()
        .map(|file| check_file(root, file, today))
        .collect();
    checked.extend(unreadable.into_iter().map(|(path, reason)| Checked {
        result: FileResult {
            path: display_path(root, &path),
            adr_id: None,
            title: None,
            ok: false,
            problems: vec![reason],
            warnings: Vec::new(),
        },
        doc: None,
    }));

    let (cloud, cloud_decisions) = cloud_decisions(args, root).await;
    let duplicates = find_duplicates(&checked, &cloud_decisions);
    CheckReport {
        files: checked.into_iter().map(|c| c.result).collect(),
        duplicates,
        skipped: skipped
            .iter()
            .map(|path| display_path(root, path))
            .collect(),
        cloud,
    }
}

fn check_file(root: &Path, file: &DecisionFile, today: &str) -> Checked {
    let checked = check_markdown(&file.body, file.is_draft(), today);
    let doc = context_file::parse(&file.body)
        .ok()
        .map(|parsed| parsed.doc);
    let (ok, problems) = match checked.result {
        Ok(_) => (true, Vec::new()),
        Err(reasons) => (false, reasons),
    };
    Checked {
        result: FileResult {
            path: display_path(root, &file.path),
            adr_id: file.adr_id().map(str::to_string),
            title: doc.as_ref().map(|doc| doc.title.clone()),
            ok,
            problems,
            warnings: checked.warnings,
        },
        doc,
    }
}

/// `path` relative to `root` when it is inside it.
fn display_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .display()
        .to_string()
}

/// The organization's decisions to compare with, for the repository when
/// one is known, and a note when the comparison was skipped or failed. Never
/// an error: the local check stands on its own.
async fn cloud_decisions(
    args: &AdrCheckArgs,
    root: &Path,
) -> (CloudComparison, Vec<ExistingDecision>) {
    let skipped = |note: String| {
        (
            CloudComparison {
                note: Some(note),
                ..CloudComparison::default()
            },
            Vec::new(),
        )
    };
    if args.no_cloud {
        return skipped("Skipped with --no-cloud.".to_string());
    }
    let session = match session(&args.api).await {
        Ok(session) => session,
        Err(ActualError::NotLoggedIn) => {
            return skipped(
                "Not signed in, so only this repository's files were compared. \
                 `actual login` adds your decisions in Actual AI."
                    .to_string(),
            )
        }
        Err(e) => return skipped(format!("Couldn't read your decisions in Actual AI: {e}")),
    };
    let (repo, scope, note) = match pick_repo(&session, args.repo.as_deref(), root).await {
        Ok(RepoPick::Chosen(repo)) => (Some(repo.id), sanitize_server_text(&repo.label), None),
        Ok(RepoPick::Unresolved(reason)) => (
            None,
            "the whole organization".to_string(),
            Some(format!("Compared with the whole organization: {reason}.")),
        ),
        Err(e) => return skipped(format!("Couldn't read your decisions in Actual AI: {e}")),
    };
    match super::cloud::decisions(&session, repo.as_deref()).await {
        Ok(decisions) => {
            let compared: Vec<ExistingDecision> = decisions
                .into_iter()
                .filter(|decision| decision.state != "archived")
                .map(|decision| ExistingDecision {
                    id: decision.id,
                    title: decision.title,
                    policies: decision.policies.into_iter().map(|p| p.statement).collect(),
                })
                .collect();
            (
                CloudComparison {
                    compared: true,
                    scope: Some(scope),
                    decisions: compared.len(),
                    note,
                },
                compared,
            )
        }
        Err(e) => skipped(format!("Couldn't read your decisions in Actual AI: {e}")),
    }
}

fn comparable(id: String, doc: &ContextFile) -> ExistingDecision {
    ExistingDecision {
        id,
        title: doc.title.clone(),
        policies: doc.policies.iter().map(|p| p.statement.clone()).collect(),
    }
}

/// Likely duplicates for each file that parses: among the files after it,
/// so each pair is reported once, and among the cloud decisions other than
/// the file's own (a synced file shares its decision's id).
fn find_duplicates(checked: &[Checked], cloud: &[ExistingDecision]) -> Vec<Duplicate> {
    let local: Vec<(usize, ExistingDecision)> = checked
        .iter()
        .enumerate()
        .filter_map(|(i, c)| Some((i, comparable(c.result.path.clone(), c.doc.as_ref()?))))
        .collect();
    let mut duplicates = Vec::new();
    for (n, (i, this)) in local.iter().enumerate() {
        let file = &checked[*i].result;
        let later: Vec<ExistingDecision> = local[n + 1..].iter().map(|(_, d)| d.clone()).collect();
        for overlap in find_overlaps(&this.title, &this.policies, &later) {
            duplicates.push(Duplicate {
                file: file.path.clone(),
                other_file: Some(overlap.adr_id.clone()),
                other_decision: None,
                overlap,
            });
        }
        let others: Vec<ExistingDecision> = cloud
            .iter()
            .filter(|d| Some(d.id.as_str()) != file.adr_id.as_deref())
            .cloned()
            .collect();
        for overlap in find_overlaps(&this.title, &this.policies, &others) {
            duplicates.push(Duplicate {
                file: file.path.clone(),
                other_file: None,
                other_decision: Some(overlap.adr_id.clone()),
                overlap,
            });
        }
    }
    duplicates
}

fn render_report(report: &CheckReport) -> String {
    let mut out = String::new();
    if report.files.is_empty() {
        out.push_str(&format!(
            "No decision files to check under {DECISION_DIR}/.\n"
        ));
    } else {
        out.push_str(&format!(
            "{} {} decision {}\n",
            theme::heading("Checked"),
            report.files.len(),
            if report.files.len() == 1 {
                "file"
            } else {
                "files"
            }
        ));
        for file in &report.files {
            let mark = if file.ok {
                theme::success(&theme::SUCCESS).to_string()
            } else {
                theme::error_prefix().to_string()
            };
            let title = file
                .title
                .as_deref()
                .map(sanitize_server_text)
                .unwrap_or_default();
            out.push_str(&format!("  {mark} {}  {title}\n", file.path));
            for problem in &file.problems {
                out.push_str(&format!("      - {problem}\n"));
            }
            for warning in &file.warnings {
                out.push_str(&format!("      {} {warning}\n", theme::warning("warning:")));
            }
        }
    }
    if !report.duplicates.is_empty() {
        out.push_str(&format!("\n{}\n", theme::heading("Likely duplicates")));
        for duplicate in &report.duplicates {
            let other = match (&duplicate.other_file, &duplicate.other_decision) {
                (Some(path), _) => path.clone(),
                (None, _) => format!(
                    "decision {} in Actual AI",
                    sanitize_server_text(&duplicate.overlap.adr_id)
                ),
            };
            out.push_str(&format!(
                "  {} and {other}: \"{}\"\n",
                duplicate.file,
                sanitize_server_text(&duplicate.overlap.title)
            ));
        }
    }
    if !report.skipped.is_empty() {
        out.push_str(&format!(
            "\nNot checked: {} {} under {DECISION_DIR}/ with no ADR header. Name a file to check it.\n",
            report.skipped.len(),
            if report.skipped.len() == 1 { "file" } else { "files" }
        ));
    }
    let cloud = &report.cloud;
    if cloud.compared {
        out.push_str(&format!(
            "\nCompared with {} {} in {}.\n",
            cloud.decisions,
            if cloud.decisions == 1 {
                "decision"
            } else {
                "decisions"
            },
            cloud.scope.as_deref().unwrap_or_default()
        ));
    }
    if let Some(note) = &cloud.note {
        out.push_str(&format!("{note}\n"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::super::tests::{api, git_repo, sign_in, CONNECTED, REPO};
    use super::*;
    use crate::cli::args::{AdrApiOptions, AdrContentArgs};
    use crate::rules::parse_rule_document;
    use crate::rules::types::RuleLevel;
    use crate::testutil::{EnvGuard, ENV_MUTEX};
    use mockito::Matcher;
    use tempfile::tempdir;

    const TODAY: &str = "2026-11-11";

    fn new_args(title: &str, policies: &[&str], scope: &[&str]) -> AdrNewArgs {
        AdrNewArgs {
            title: title.to_string(),
            content: AdrContentArgs {
                context: vec!["We need one relational store.".to_string()],
                policies: policies.iter().map(|p| p.to_string()).collect(),
            },
            deciders: Some("Ana".to_string()),
            scope: scope.iter().map(|s| s.to_string()).collect(),
            status: None,
            root: None,
            json: false,
        }
    }

    fn check_args(files: &[PathBuf]) -> AdrCheckArgs {
        AdrCheckArgs {
            files: files.to_vec(),
            no_cloud: true,
            repo: None,
            root: None,
            api: AdrApiOptions::default(),
        }
    }

    fn block<T>(future: impl std::future::Future<Output = T>) -> T {
        runtime().unwrap().block_on(future)
    }

    #[test]
    fn new_writes_a_pair_the_checks_enforce() {
        let tmp = tempdir().unwrap();
        let args = new_args(
            "Use Postgres for storage",
            &[
                "MUST: Store relational data in Postgres.",
                "MUST NOT: Add a second database.",
            ],
            &["src/**"],
        );
        let written = write_new(&args, tmp.path(), true, TODAY).unwrap();
        assert_eq!(written.status, SaveStatus::Accepted);
        assert!(written
            .decision_file
            .starts_with(tmp.path().join(DECISION_DIR)));
        let name = written.decision_file.file_name().unwrap().to_str().unwrap();
        assert_eq!(
            name,
            decision_doc_file_name(&written.adr_id, "Use Postgres for storage")
        );

        let doc = files::read_decision_file(&written.decision_file).unwrap();
        assert_eq!(doc.adr_id(), Some(written.adr_id.as_str()));
        assert_eq!(doc.meta.as_ref().unwrap().paths, ["src/**"]);
        let parsed = context_file::parse(&doc.body).unwrap().doc;
        assert_eq!(parsed.title, "Use Postgres for storage");
        assert_eq!(parsed.policies.len(), 2);

        let rule_text = std::fs::read_to_string(&written.rule_file).unwrap();
        let rules = parse_rule_document(&written.rule_file, &rule_text).unwrap();
        assert!(rules.is_active());
        assert_eq!(rules.rules.len(), 2, "{rule_text}");
        assert_eq!(rules.rules[1].level, RuleLevel::MustNot);
        assert!(rules.warnings.is_empty(), "{:?}", rules.warnings);
        assert_eq!(
            rules.meta.as_ref().unwrap().adr_id.as_deref(),
            Some(written.adr_id.as_str())
        );
        assert!(render_written(&written).contains("enforce it now"));
    }

    #[test]
    fn new_without_a_person_at_the_terminal_writes_a_draft() {
        let tmp = tempdir().unwrap();
        let args = new_args("Page every list read", &["MUST: Bound each read."], &[]);
        let written = write_new(&args, tmp.path(), false, TODAY).unwrap();
        assert_eq!(written.status, SaveStatus::Draft);
        let rule_text = std::fs::read_to_string(&written.rule_file).unwrap();
        let rules = parse_rule_document(&written.rule_file, &rule_text).unwrap();
        assert!(!rules.is_active(), "a draft is enforced by no check");
        assert!(render_written(&written).contains("a draft"));
    }

    #[test]
    fn new_refuses_a_decision_the_save_check_fails() {
        let tmp = tempdir().unwrap();
        let mut args = new_args("  ", &[], &[]);
        args.content.context.clear();
        let err = write_new(&args, tmp.path(), true, TODAY).err();
        assert!(
            matches!(&err, Some(ActualError::AdrInvalid { reasons })
                if reasons == &["Add a title.", "Add at least one policy."]),
            "{err:?}"
        );
        let bad = new_args("Use Postgres", &["Use it."], &[]);
        assert!(matches!(
            write_new(&bad, tmp.path(), true, TODAY),
            Err(ActualError::AdrInvalid { .. })
        ));
        assert!(!tmp.path().join(DECISION_DIR).exists());
    }

    #[test]
    fn new_leaves_no_half_pair_behind() {
        let tmp = tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join(".actual")).unwrap();
        // A file where the rules directory should be.
        std::fs::write(tmp.path().join(RULES_DIR), "").unwrap();
        let args = new_args("Use Postgres", &["MUST: Use it."], &[]);
        assert!(write_new(&args, tmp.path(), true, TODAY).is_err());
        let docs: Vec<_> = std::fs::read_dir(tmp.path().join(DECISION_DIR))
            .unwrap()
            .collect();
        assert!(docs.is_empty(), "the decision doc was removed again");

        let blocked = tempdir().unwrap();
        std::fs::create_dir_all(blocked.path().join("docs")).unwrap();
        std::fs::write(blocked.path().join(DECISION_DIR), "").unwrap();
        assert!(write_new(&args, blocked.path(), true, TODAY).is_err());
    }

    #[test]
    fn write_new_file_never_replaces_a_file() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("a.md");
        write_new_file(&path, "one").unwrap();
        assert!(write_new_file(&path, "two").is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "one");
    }

    #[test]
    fn exec_new_prints_text_or_json() {
        let tmp = tempdir().unwrap();
        let mut args = new_args("Use Postgres", &["MUST: Use it."], &[]);
        args.root = Some(tmp.path().to_path_buf());
        exec_new(&args).unwrap();
        args.json = true;
        args.title = "Use Redis for caching".to_string();
        exec_new(&args).unwrap();
        let docs: Vec<_> = std::fs::read_dir(tmp.path().join(DECISION_DIR))
            .unwrap()
            .collect();
        assert_eq!(docs.len(), 2);
        assert!(exec_new(&new_args("", &[], &[])).is_err());
    }

    /// A repository with a good pair, a decision file that fails the check,
    /// a README with no header and an unreadable file.
    fn mixed_repository(root: &Path) -> PathBuf {
        let good = write_new(
            &new_args(
                "Use Postgres for storage",
                &["MUST: Store relational data in Postgres."],
                &[],
            ),
            root,
            true,
            TODAY,
        )
        .unwrap();
        let bad = root.join(DECISION_DIR).join("bad.md");
        std::fs::write(
            &bad,
            "---\nversion: 2\nmetadata:\n  actual:\n    adr_id: bad-1\n    status: draft\n---\n\n# Bad\n\n## Context\n\n- Why.\n",
        )
        .unwrap();
        std::fs::write(root.join(DECISION_DIR).join("README.md"), "# Decisions\n").unwrap();
        std::fs::write(root.join(DECISION_DIR).join("binary.md"), [0xff, 0xfe]).unwrap();
        good.decision_file
    }

    #[test]
    fn check_reports_each_file_and_what_it_skipped() {
        let tmp = tempdir().unwrap();
        mixed_repository(tmp.path());
        let report = block(run_check(&check_args(&[]), tmp.path(), TODAY));
        assert_eq!(report.files.len(), 3, "{:#?}", report.files);
        assert_eq!(report.failed(), 2);
        let bad = report
            .files
            .iter()
            .find(|f| f.path.ends_with("bad.md"))
            .unwrap();
        assert_eq!(bad.adr_id.as_deref(), Some("bad-1"));
        assert!(!bad.problems.is_empty());
        assert_eq!(report.skipped, [format!("{DECISION_DIR}/README.md")]);
        assert_eq!(
            report.cloud.note.as_deref(),
            Some("Skipped with --no-cloud.")
        );

        let text = render_report(&report);
        assert!(text.contains("Checked 3 decision files"), "{text}");
        assert!(
            text.contains("Not checked: 1 file under docs/adr/"),
            "{text}"
        );
        assert!(text.contains("Skipped with --no-cloud."));
    }

    #[test]
    fn check_of_named_files_reads_only_those() {
        let tmp = tempdir().unwrap();
        let good = mixed_repository(tmp.path());
        let missing = tmp.path().join("missing.md");
        let report = block(run_check(
            &check_args(&[good.clone(), missing]),
            tmp.path(),
            TODAY,
        ));
        assert_eq!(report.files.len(), 2);
        assert!(report.files[0].ok);
        assert!(!report.files[1].ok);
        assert!(report.skipped.is_empty());
        let text = render_report(&report);
        assert!(text.contains("Checked 2 decision files"), "{text}");
    }

    #[test]
    fn check_lists_each_local_duplicate_pair_once() {
        let tmp = tempdir().unwrap();
        for _ in 0..2 {
            write_new(
                &new_args(
                    "Use Postgres for storage",
                    &["MUST: Store relational data in Postgres."],
                    &[],
                ),
                tmp.path(),
                true,
                TODAY,
            )
            .unwrap();
        }
        let report = block(run_check(&check_args(&[]), tmp.path(), TODAY));
        assert_eq!(report.duplicates.len(), 1, "{:#?}", report.duplicates);
        assert!(report.duplicates[0].other_file.is_some());
        let text = render_report(&report);
        assert!(text.contains("Likely duplicates"), "{text}");
        assert!(text.contains(" and docs/adr/"), "{text}");
    }

    #[test]
    fn check_of_an_empty_repository_passes() {
        let tmp = tempdir().unwrap();
        let mut args = check_args(&[]);
        args.root = Some(tmp.path().to_path_buf());
        exec_check(&args).unwrap();
        args.api.json = true;
        exec_check(&args).unwrap();
        let report = block(run_check(&args, tmp.path(), TODAY));
        assert_eq!(
            render_report(&report).lines().next(),
            Some("No decision files to check under docs/adr/.")
        );
    }

    #[test]
    fn exec_check_fails_when_a_file_fails() {
        let tmp = tempdir().unwrap();
        mixed_repository(tmp.path());
        let mut args = check_args(&[]);
        args.root = Some(tmp.path().to_path_buf());
        let err = exec_check(&args).err();
        assert!(
            matches!(&err, Some(ActualError::AdrCheckFailed(message)) if message == "2 of 3 decision files failed the check"),
            "{err:?}"
        );
    }

    #[test]
    fn check_compares_with_cloud_decisions_but_not_a_files_own() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let config = tempdir().unwrap();
        let _guards = sign_in(config.path());
        let tmp = tempdir().unwrap();
        let written = write_new(
            &new_args(
                "Use Postgres for storage",
                &["MUST: Store relational data in Postgres."],
                &[],
            ),
            tmp.path(),
            true,
            TODAY,
        )
        .unwrap();
        let decision = |id: &str, title: &str, status: &str| {
            format!(
                r#"{{"id":"{id}","title":"{title}","status":"{status}","policyRules":[{{"id":"r","adrId":"{id}","level":"MUST","statement":"Store relational data in Postgres."}}]}}"#
            )
        };
        let mut server = mockito::Server::new();
        let list = server
            .mock("GET", "/v1/gcp/product/decisions")
            .match_query(Matcher::UrlEncoded("repository_id".into(), REPO.into()))
            .with_status(200)
            .with_body(format!(
                r#"{{"adrs":[{},{},{}],"suggestions":[]}}"#,
                decision(&written.adr_id, "Use Postgres for storage", "accepted"),
                decision("cloud-2", "Postgres for storage", "draft"),
                decision("cloud-3", "Use Postgres for storage", "deleted"),
            ))
            .create();
        let mut args = check_args(&[]);
        args.no_cloud = false;
        args.repo = Some(REPO.to_string());
        args.api = api(&server);
        let report = block(run_check(&args, tmp.path(), TODAY));
        list.assert();
        assert!(report.cloud.compared);
        // The archived cloud-3 isn't compared, so it isn't counted.
        assert_eq!(report.cloud.decisions, 2);
        let others: Vec<_> = report
            .duplicates
            .iter()
            .filter_map(|d| d.other_decision.as_deref())
            .collect();
        assert_eq!(others, ["cloud-2"]);
        let text = render_report(&report);
        assert!(text.contains("decision cloud-2 in Actual AI"), "{text}");
        assert!(
            text.contains(&format!("Compared with 2 decisions in {REPO}.")),
            "{text}"
        );
    }

    #[test]
    fn check_compares_with_the_organization_when_no_repository_is_known() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let config = tempdir().unwrap();
        let _guards = sign_in(config.path());
        let tmp = tempdir().unwrap();
        let mut server = mockito::Server::new();
        let org = server
            .mock("GET", "/v1/gcp/product/decisions")
            .match_query(Matcher::Any)
            .with_status(200)
            .with_body(r#"{"adrs":[{"id":"c1","title":"T","status":"accepted"}],"suggestions":[]}"#)
            .create();
        let mut args = check_args(&[]);
        args.no_cloud = false;
        args.api = api(&server);
        let report = block(run_check(&args, tmp.path(), TODAY));
        org.assert();
        assert!(report.cloud.compared);
        assert_eq!(
            report.cloud.scope.as_deref(),
            Some("the whole organization")
        );
        let text = render_report(&report);
        assert!(
            text.contains("Compared with 1 decision in the whole organization."),
            "{text}"
        );
        assert!(text.contains("no origin remote"), "{text}");
    }

    #[test]
    fn check_notes_why_it_skipped_the_cloud() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempdir().unwrap();
        let mut args = check_args(&[]);
        args.no_cloud = false;

        // Signed out.
        let empty = tempdir().unwrap();
        let _g1 = EnvGuard::remove("ACTUAL_CONFIG");
        let g2 = EnvGuard::set("ACTUAL_CONFIG_DIR", empty.path().to_str().unwrap());
        let report = block(run_check(&args, tmp.path(), TODAY));
        assert!(report.cloud.note.unwrap().contains("Not signed in"));
        drop(g2);

        // A credentials file that can't be read.
        let broken = tempdir().unwrap();
        let _g3 = EnvGuard::set("ACTUAL_CONFIG_DIR", broken.path().to_str().unwrap());
        let creds = crate::auth::store::credentials_path().unwrap();
        std::fs::create_dir_all(creds.parent().unwrap()).unwrap();
        std::fs::write(&creds, "not json").unwrap();
        let report = block(run_check(&args, tmp.path(), TODAY));
        assert!(report
            .cloud
            .note
            .unwrap()
            .starts_with("Couldn't read your decisions"));
    }

    #[test]
    fn check_notes_a_repository_or_list_it_could_not_read() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let config = tempdir().unwrap();
        let _guards = sign_in(config.path());
        let tmp = tempdir().unwrap();
        git_repo(
            tmp.path(),
            Some("git@github.com:actual-software/actual-cli.git"),
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
            .with_status(500)
            .create();
        let mut args = check_args(&[]);
        args.no_cloud = false;
        args.api = api(&server);
        let report = block(run_check(&args, tmp.path(), TODAY));
        assert!(!report.cloud.compared);
        assert!(report.cloud.note.unwrap().contains("HTTP 500"));

        args.repo = Some("not-connected".to_string());
        let report = block(run_check(&args, tmp.path(), TODAY));
        assert!(report
            .cloud
            .note
            .unwrap()
            .contains("No connected repository"));
    }

    #[test]
    fn render_report_uses_singular_counts() {
        let report = CheckReport {
            files: vec![FileResult {
                path: "docs/adr/a.md".to_string(),
                adr_id: None,
                title: None,
                ok: true,
                problems: Vec::new(),
                warnings: vec!["No title".to_string()],
            }],
            duplicates: Vec::new(),
            skipped: vec!["a".to_string(), "b".to_string()],
            cloud: CloudComparison {
                compared: true,
                scope: Some("o/r".to_string()),
                decisions: 1,
                note: None,
            },
        };
        let text = render_report(&report);
        assert!(text.contains("Checked 1 decision file\n"), "{text}");
        assert!(text.contains("warning: No title"), "{text}");
        assert!(text.contains("2 files under"), "{text}");
        assert!(text.contains("Compared with 1 decision in o/r."), "{text}");
    }
}
