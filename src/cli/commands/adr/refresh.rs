//! `actual adr refresh`: bring `.actual/rules/` in step with the
//! repository's decisions, as the plugin's hooks do before each call, and
//! report the files under `docs/adr/` that aren't normalized when that list
//! changes.
//!
//! Signed out it reads `docs/adr/`. Signed in it reads the repository's
//! decisions in Actual AI within `--timeout-ms`, and leaves every rule file
//! as it is when Actual AI is slow or can't be reached. It always exits 0,
//! so a hook that runs it can't fail on it; what went wrong is in the
//! report.
//!
//! It never renews a sign-in itself. Actual AI rotates refresh tokens, so a
//! renewal cut off by the time box, or by the hook's own timeout, would spend
//! the stored refresh token and sign the person out. An access token about to
//! expire starts a detached `actual whoami --check` instead, which renews it
//! under the store's lock and finishes after this process is gone, and the
//! rule files stay as they are until the next refresh.
//!
//! What the refresh remembers about a checkout, [`Record`], lives under the
//! user's config directory, one file per checkout, written under the
//! directory's lock as [`brief_ledger`] writes, so two hooks at once keep
//! each other's entries. A record that can't be read starts empty, which
//! only means existing rule files are met again.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Serialize;

use super::{json_text, pick_repo, runtime, today, RepoPick, Session};
use crate::adr::context_file::Policy;
use crate::adr::files::{rule_files_by_id, DECISION_DIR, RULES_DIR};
use crate::adr::header::REPOSITORY_ROOT;
use crate::adr::refresh::{
    self as core, Applied, ConnectedRepo, Origin, Planner, Record, RuleSource, Unnormalized,
    RECORD_FORMAT,
};
use crate::api::adr_types::Decision;
use crate::api::{resolve_api_url, ActualApiClient};
use crate::auth::refresh::needs_refresh;
use crate::auth::store::{self, StoredCredentials};
use crate::cli::args::{AdrApiOptions, AdrRefreshArgs};
use crate::cli::commands::advisor::origin_remote_url;
use crate::cli::commands::brief_ledger;
use crate::cli::commands::sync::resolve_cwd;
use crate::cli::ui::theme;
use crate::error::ActualError;

/// The directory under the config directory the records live in.
const DIR_NAME: &str = "adr-refresh";

/// How long a record write waits for another refresh's.
const LOCK_WAIT: Duration = Duration::from_millis(500);

/// Which decisions the refresh followed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(super) enum Mode {
    /// The files under `docs/adr/`.
    SignedOut,
    /// The repository's decisions in Actual AI.
    SignedIn,
}

/// What a refresh did, for `--json`. `message` is set when a person should
/// hear about it: rule files changed, or the unnormalized list did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(super) struct Report {
    pub(super) mode: Mode,
    pub(super) written: Vec<String>,
    pub(super) rewritten: Vec<String>,
    pub(super) removed: Vec<String>,
    /// Signed in, whether `.actual/rules/` was left as it is because Actual
    /// AI couldn't be read in time.
    pub(super) kept: bool,
    pub(super) unnormalized: Vec<Unnormalized>,
    pub(super) unnormalized_changed: bool,
    pub(super) message: Option<String>,
    pub(super) problems: Vec<String>,
}

pub(super) fn exec_refresh(args: &AdrRefreshArgs) -> Result<(), ActualError> {
    let root = args.root.clone().unwrap_or_else(resolve_cwd);
    let report = refresh(&root, args, &today(), record_dir().as_deref());
    if args.api.json {
        println!("{}", json_text(&report));
    } else {
        print!("{}", render(&report));
    }
    Ok(())
}

/// Refresh `root`'s rule files and check its unnormalized list, keeping the
/// record under `dir` when there is one.
pub(super) fn refresh(
    root: &Path,
    args: &AdrRefreshArgs,
    today: &str,
    dir: Option<&Path>,
) -> Report {
    let record = dir.map(|dir| load(dir, root)).unwrap_or_default();
    let scan = core::scan_docs(root, today);
    let on_disk = rule_files_by_id(root);
    let mut problems = Vec::new();
    let mut repo = record.connected_repo.clone();
    let (mode, plan) = match store::load() {
        Ok(None) => (
            Mode::SignedOut,
            Some(core::plan_from_docs(root, &scan, &on_disk, &record)),
        ),
        Ok(Some(creds)) if needs_refresh(&creds) => {
            spawn_renewal();
            problems.push(
                "Your sign-in is being renewed in the background, so the rule files were left \
                 as they are until the next refresh."
                    .to_string(),
            );
            (Mode::SignedIn, None)
        }
        Ok(Some(creds)) => {
            match read_cloud(root, &creds, &args.api, args.timeout_ms, repo.as_ref()) {
                Ok((decisions, connected)) => {
                    repo = Some(connected);
                    let sources = rule_sources(&decisions);
                    (
                        Mode::SignedIn,
                        Some(core::plan_from_cloud(root, &sources, &on_disk, &record)),
                    )
                }
                Err(problem) => {
                    problems.push(problem);
                    repo = None;
                    (Mode::SignedIn, None)
                }
            }
        }
        Err(e) => {
            problems.push(format!(
                "Couldn't read your sign-in ({e}), so the rule files were left as they are."
            ));
            (Mode::SignedIn, None)
        }
    };
    let kept = plan.is_none();
    let (applied, set, drop) = core::apply(root, plan.unwrap_or_default());
    let list = core::list_fingerprint(&scan.unnormalized);
    let unnormalized_changed = match &record.unnormalized {
        Some(last) => *last != list,
        None => !scan.unnormalized.is_empty(),
    };
    if let Some(dir) = dir {
        save(dir, root, |record| {
            record.apply_edits(&set, &drop);
            record.unnormalized = Some(list);
            record.connected_repo = repo;
        });
    }
    problems.extend(applied.problems.iter().cloned());
    let message = message(
        mode,
        &applied,
        unnormalized_changed.then_some(scan.unnormalized.as_slice()),
    );
    Report {
        mode,
        written: applied.written,
        rewritten: applied.rewritten,
        removed: applied.removed,
        kept,
        unnormalized: scan.unnormalized,
        unnormalized_changed,
        message,
        problems,
    }
}

/// Bring decision `adr_id`'s rule file in step with its file under
/// `docs/adr/` at once, as each signed-out change does. `changed` says the
/// caller changed what the rule file says, so a file the record doesn't know
/// is rewritten too. A decision whose file is gone, or no longer governs,
/// loses whatever rule file names it.
pub(super) fn refresh_decision(
    root: &Path,
    adr_id: &str,
    today: &str,
    dir: Option<&Path>,
    changed: bool,
) -> Applied {
    let scan = core::scan_docs(root, today);
    if let Some(file) = scan
        .unnormalized
        .iter()
        .find(|file| file.adr_id.as_deref() == Some(adr_id))
    {
        // As on a hook's refresh, the rule file stays until the file reads again.
        return Applied {
            problems: vec![format!(
                "{} isn't normalized ({}), so its rule file was left as it is",
                file.path,
                file.reasons.join(" ")
            )],
            ..Applied::default()
        };
    }
    let record = dir.map(|dir| load(dir, root)).unwrap_or_default();
    let on_disk = rule_files_by_id(root);
    let mut planner = Planner::new(root, &on_disk, &record);
    let doc = scan
        .decisions
        .iter()
        .find(|doc| doc.source.adr_id == adr_id);
    let source = doc.filter(|doc| doc.governs).map(|doc| &doc.source);
    planner.decision(adr_id, source, Origin::DocsAdr, changed);
    let (applied, set, drop) = core::apply(root, planner.plan);
    if let Some(dir) = dir {
        save(dir, root, |record| record.apply_edits(&set, &drop));
    }
    applied
}

// ── Actual AI ──────────────────────────────────────────────────────────────

/// The repository's decisions in Actual AI, read within `timeout_ms`, and
/// the connected repository they came from; or why they couldn't be read.
fn read_cloud(
    root: &Path,
    creds: &StoredCredentials,
    api: &AdrApiOptions,
    timeout_ms: u64,
    cached: Option<&ConnectedRepo>,
) -> Result<(Vec<Decision>, ConnectedRepo), String> {
    let kept = "so the rule files were left as they are";
    let rt = runtime().map_err(|e| format!("Couldn't read Actual AI ({e}), {kept}."))?;
    rt.block_on(async {
        match tokio::time::timeout(
            Duration::from_millis(timeout_ms),
            read_decisions(root, creds, api, cached),
        )
        .await
        {
            Ok(Ok(read)) => Ok(read),
            Ok(Err(reason)) => Err(format!("{reason}, {kept}.")),
            Err(_) => Err(format!(
                "Actual AI didn't answer within {timeout_ms} ms, {kept}."
            )),
        }
    })
}

async fn read_decisions(
    root: &Path,
    creds: &StoredCredentials,
    api: &AdrApiOptions,
    cached: Option<&ConnectedRepo>,
) -> Result<(Vec<Decision>, ConnectedRepo), String> {
    let unread = |e: ActualError| format!("Couldn't read your decisions in Actual AI ({e})");
    let session = Session {
        client: ActualApiClient::new(&resolve_api_url(api.api_url.as_deref()))
            .map_err(unread)?
            .with_bearer(&creds.access_token),
        org_id: creds.organization_id.clone(),
    };
    let remote = origin_remote_url(root).await.unwrap_or_default();
    let reuse = cached.filter(|repo| {
        !remote.is_empty() && repo.remote == remote && repo.org_id == session.org_id
    });
    let repo_id = match reuse {
        Some(repo) => repo.repo_id.clone(),
        None => match pick_repo(&session, None, root).await.map_err(unread)? {
            RepoPick::Chosen(repo) => repo.id,
            RepoPick::Unresolved(reason) => {
                return Err(format!(
                    "No connected repository to read decisions for: {reason}"
                ))
            }
        },
    };
    let decisions = super::cloud::decisions(&session, Some(&repo_id))
        .await
        .map_err(unread)?;
    Ok((
        decisions,
        ConnectedRepo {
            org_id: session.org_id,
            remote,
            repo_id,
        },
    ))
}

/// Renew the stored sign-in in a detached `actual whoami --check`, in its own
/// process group as the telemetry uploader is, so the renewal outlives this
/// process and a hook that kills its group. Best effort: a renewal that can't
/// start leaves the next signed-in command to renew.
#[cfg(not(test))]
fn spawn_renewal() {
    if let Ok(exe) = std::env::current_exe() {
        let mut child = std::process::Command::new(exe);
        child
            .args(["whoami", "--check"])
            .env(crate::update_check::OPT_OUT_ENV, "1")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        #[cfg(unix)]
        std::os::unix::process::CommandExt::process_group(&mut child, 0);
        let _ = child.spawn();
    }
}

/// The test binary isn't `actual`, so a test only counts the renewals.
#[cfg(test)]
fn spawn_renewal() {
    tests::RENEWALS.with(|count| count.set(count.get() + 1));
}

/// Each decision's id, with what its rule file says while it is active.
fn rule_sources(decisions: &[Decision]) -> Vec<(String, Option<RuleSource>)> {
    decisions
        .iter()
        .map(|decision| {
            let source = (decision.state == "active").then(|| RuleSource {
                adr_id: decision.id.clone(),
                title: decision.title.clone(),
                status: decision.status.clone(),
                scope_root: decision
                    .scope
                    .as_ref()
                    .map_or(REPOSITORY_ROOT, |scope| scope.root.as_str())
                    .to_string(),
                scope_paths: decision
                    .scope
                    .as_ref()
                    .map(|scope| scope.paths.clone())
                    .unwrap_or_default(),
                policies: decision
                    .policies
                    .iter()
                    .map(|policy| Policy {
                        level: policy.level,
                        statement: policy.statement.clone(),
                    })
                    .collect(),
            });
            (decision.id.clone(), source)
        })
        .collect()
}

// ── the record ─────────────────────────────────────────────────────────────

/// The directory the records live in, under the user's config directory.
pub(super) fn record_dir() -> Option<PathBuf> {
    crate::config::paths::config_dir()
        .ok()
        .map(|dir| dir.join(DIR_NAME))
}

/// The record file for checkout `root`, named from its full path.
fn record_path(dir: &Path, root: &Path) -> PathBuf {
    let root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let name = core::sha256_hex(root.to_string_lossy().as_bytes());
    dir.join(format!("{name}.json"))
}

/// The record for `root`, or an empty one when it is missing, damaged or
/// another format's. A read takes no lock: every write replaces the file
/// whole.
pub(super) fn load(dir: &Path, root: &Path) -> Record {
    std::fs::read_to_string(record_path(dir, root))
        .ok()
        .and_then(|text| serde_json::from_str::<Record>(&text).ok())
        .filter(|record| record.format_version == RECORD_FORMAT)
        .unwrap_or_default()
}

/// Apply `change` to the record for `root` under the directory's lock, best
/// effort, writing only a record that changed. Whether the record now holds
/// the change.
fn save(dir: &Path, root: &Path, change: impl FnOnce(&mut Record)) -> bool {
    if std::fs::create_dir_all(dir).is_err() {
        return false;
    }
    let Some(_lock) = brief_ledger::lock(dir, LOCK_WAIT) else {
        return false;
    };
    let before = load(dir, root);
    let mut record = before.clone();
    change(&mut record);
    record.format_version = RECORD_FORMAT;
    if record == before {
        return true;
    }
    serde_json::to_string(&record).is_ok_and(|json| {
        crate::config::paths::write_secure(&record_path(dir, root), json.as_bytes()).is_ok()
    })
}

// ── output ─────────────────────────────────────────────────────────────────

/// What a person should hear about a refresh: the rule files it changed and,
/// when the list changed, the files that aren't normalized.
fn message(mode: Mode, applied: &Applied, unnormalized: Option<&[Unnormalized]>) -> Option<String> {
    let mut lines = Vec::new();
    if applied.changed() > 0 {
        let from = match mode {
            Mode::SignedOut => format!("{DECISION_DIR}/"),
            Mode::SignedIn => "your decisions in Actual AI".to_string(),
        };
        let mut counts = Vec::new();
        for (verb, paths) in [
            ("wrote", &applied.written),
            ("rewrote", &applied.rewritten),
            ("removed", &applied.removed),
        ] {
            if !paths.is_empty() {
                counts.push(format!("{verb} {}", paths.len()));
            }
        }
        lines.push(format!(
            "Updated {RULES_DIR}/ from {from}: {} rule {}.",
            counts.join(", "),
            if applied.changed() == 1 {
                "file"
            } else {
                "files"
            }
        ));
    }
    match unnormalized {
        Some([]) => lines.push(format!(
            "Every decision file under {DECISION_DIR}/ is normalized now."
        )),
        Some(list) => {
            lines.push(format!(
                "{} {} under {DECISION_DIR}/ {} normalized, so {} no rule file:",
                list.len(),
                if list.len() == 1 { "file" } else { "files" },
                if list.len() == 1 { "isn't" } else { "aren't" },
                if list.len() == 1 {
                    "it gets"
                } else {
                    "they get"
                },
            ));
            for file in list {
                lines.push(format!("  {}: {}", file.path, file.reasons.join(" ")));
            }
            lines.push(
                "Normalize each one: a version 2 ADR header naming its id, and a body that \
                 passes `actual adr check`, as `actual adr create` writes."
                    .to_string(),
            );
        }
        None => {}
    }
    (!lines.is_empty()).then(|| lines.join("\n"))
}

fn render(report: &Report) -> String {
    let mut out = match &report.message {
        Some(message) => format!("{message}\n"),
        None if report.kept => String::new(),
        None => format!(
            "{RULES_DIR}/ already matches {}.\n",
            match report.mode {
                Mode::SignedOut => format!("{DECISION_DIR}/"),
                Mode::SignedIn => "your decisions in Actual AI".to_string(),
            }
        ),
    };
    for problem in &report.problems {
        out.push_str(&format!("{} {problem}\n", theme::warning("warning:")));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::super::tests::{git_repo, sign_in, CONNECTED, ORG, REPO};
    use super::*;
    use crate::adr::files::read_decision_file;
    use crate::adr::names::rule_file_name;
    use crate::testutil::{EnvGuard, ENV_MUTEX};
    use mockito::Matcher;
    use tempfile::tempdir;

    const TODAY: &str = "2026-11-20";
    const ID: &str = "0b6f7e2a-1111-4111-8111-111111111111";

    thread_local! {
        /// How many renewals this thread's refreshes started.
        pub(super) static RENEWALS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    }

    fn decision_file(id: &str, status: &str, title: &str, policy: &str) -> String {
        format!(
            "---\nversion: 2\nname: {id}-x\nmetadata:\n  actual:\n    adr_id: {id}\n    status: {status}\n  scope:\n    root: .\n---\n\n# {title}\n\nStatus: accepted\nDate: 2026-11-19\nDeciders: \n\n## Context\n\n- Why.\n\n## Decision\n\n1. {policy}\n"
        )
    }

    fn write(root: &Path, relative: &str, text: &str) -> PathBuf {
        let path = root.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, text).unwrap();
        path
    }

    fn args(server: Option<&mockito::Server>) -> AdrRefreshArgs {
        AdrRefreshArgs {
            root: None,
            timeout_ms: 5_000,
            api: AdrApiOptions {
                api_url: server.map(mockito::Server::url),
                json: true,
            },
        }
    }

    /// Signed out: a config directory with no sign-in, which the guards
    /// point at.
    fn signed_out(config: &Path) -> (EnvGuard, EnvGuard) {
        (
            EnvGuard::remove("ACTUAL_CONFIG"),
            EnvGuard::set("ACTUAL_CONFIG_DIR", config.to_str().unwrap()),
        )
    }

    #[test]
    fn signed_out_refresh_follows_docs_adr_and_reports_the_list_when_it_changes() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let config = tempdir().unwrap();
        let _guards = signed_out(config.path());
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        let records = config.path().join(DIR_NAME);
        write(
            root,
            "docs/adr/a.md",
            &decision_file(ID, "accepted", "Use Postgres", "MUST: Use Postgres."),
        );
        let plain = write(root, "docs/adr/plain.md", "# Use Kafka\n");

        let first = refresh(root, &args(None), TODAY, Some(&records));
        assert_eq!(first.mode, Mode::SignedOut);
        assert_eq!(
            first.written,
            [format!(
                "{RULES_DIR}/{}",
                rule_file_name(ID, "Use Postgres")
            )]
        );
        assert!(first.unnormalized_changed);
        let message = first.message.clone().unwrap();
        assert!(
            message.contains("Updated .actual/rules/ from docs/adr/: wrote 1 rule file."),
            "{message}"
        );
        assert!(
            message.contains("1 file under docs/adr/ isn't normalized, so it gets no rule file:"),
            "{message}"
        );
        assert!(
            message.contains("  docs/adr/plain.md: It has no ADR header."),
            "{message}"
        );
        assert!(render(&first).starts_with("Updated"));

        // The same state again: nothing to say.
        let quiet = refresh(root, &args(None), TODAY, Some(&records));
        assert!(quiet.written.is_empty() && !quiet.unnormalized_changed);
        assert_eq!(quiet.message, None);
        assert_eq!(quiet.unnormalized.len(), 1);
        assert_eq!(
            render(&quiet),
            ".actual/rules/ already matches docs/adr/.\n"
        );

        // The list empties: said once.
        std::fs::remove_file(plain).unwrap();
        let fixed = refresh(root, &args(None), TODAY, Some(&records));
        assert_eq!(
            fixed.message.as_deref(),
            Some("Every decision file under docs/adr/ is normalized now.")
        );
        let json = json_text(&fixed);
        assert!(json.contains("\"mode\": \"signed-out\""), "{json}");
    }

    #[test]
    fn a_refresh_without_a_record_still_writes_and_reports() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let config = tempdir().unwrap();
        let _guards = signed_out(config.path());
        let tmp = tempdir().unwrap();
        write(
            tmp.path(),
            "docs/adr/a.md",
            &decision_file(ID, "accepted", "Use Postgres", "MUST: Use Postgres."),
        );
        let report = refresh(tmp.path(), &args(None), TODAY, None);
        assert_eq!(report.written.len(), 1);
        assert!(!report.kept);
    }

    #[test]
    fn a_damaged_record_reads_as_empty_and_the_next_save_replaces_it() {
        let dir = tempdir().unwrap();
        let root = tempdir().unwrap();
        std::fs::create_dir_all(dir.path()).unwrap();
        std::fs::write(record_path(dir.path(), root.path()), "not json").unwrap();
        assert_eq!(load(dir.path(), root.path()), Record::default());
        assert!(save(dir.path(), root.path(), |record| {
            record.unnormalized = Some("x".to_string());
        }));
        let record = load(dir.path(), root.path());
        assert_eq!(record.format_version, RECORD_FORMAT);
        assert_eq!(record.unnormalized.as_deref(), Some("x"));
        // Another checkout has its own record.
        let other = tempdir().unwrap();
        assert_eq!(load(dir.path(), other.path()), Record::default());
        // A file where the directory should be: nothing is written.
        let blocked = dir.path().join("blocked");
        std::fs::write(&blocked, "").unwrap();
        assert!(!save(&blocked, root.path(), |_| {}));
    }

    /// One active decision, one archived, for the connected repository.
    const DECISIONS: &str = r#"{"adrs":[
        {"id":"0b6f7e2a-1111-4111-8111-111111111111","title":"Use Postgres","status":"accepted","scope":{"root":".","paths":["src/**"]},"policyRules":[{"id":"r1","adrId":"0b6f7e2a-1111-4111-8111-111111111111","level":"MUST_NOT","statement":"Add a second database."}]},
        {"id":"archived-1","title":"Old","status":"deleted","policyRules":[{"id":"r2","adrId":"archived-1","level":"MUST","statement":"Old."}]}
    ],"suggestions":[]}"#;

    #[test]
    fn signed_in_refresh_writes_active_decisions_and_reuses_the_repository_it_found() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let config = tempdir().unwrap();
        let _guards = sign_in(config.path());
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        git_repo(root, Some("git@github.com:actual-software/actual-cli.git"));
        let archived = write(
            root,
            ".actual/rules/old.md",
            "---\nversion: 2\nmetadata:\n  actual:\n    adr_id: archived-1\n---\n# Old\n",
        );
        let mut server = mockito::Server::new();
        let repos = server
            .mock("GET", "/v1/connected-repos")
            .match_query(Matcher::Any)
            .with_status(200)
            .with_body(CONNECTED)
            .expect(1)
            .create();
        let list = server
            .mock("GET", "/v1/gcp/product/decisions")
            .match_query(Matcher::UrlEncoded("repository_id".into(), REPO.into()))
            .with_status(200)
            .with_body(DECISIONS)
            .expect(2)
            .create();
        let records = config.path().join(DIR_NAME);

        let first = refresh(root, &args(Some(&server)), TODAY, Some(&records));
        assert_eq!(first.mode, Mode::SignedIn, "{first:?}");
        assert!(first.problems.is_empty(), "{first:?}");
        let name = rule_file_name(ID, "Use Postgres");
        assert_eq!(first.written, [format!("{RULES_DIR}/{name}")]);
        assert_eq!(first.removed, [".actual/rules/old.md"]);
        assert!(!archived.exists());
        let rule = read_decision_file(&root.join(RULES_DIR).join(&name)).unwrap();
        assert_eq!(rule.adr_id(), Some(ID));
        assert_eq!(rule.meta.unwrap().paths, ["src/**"]);
        assert!(first
            .message
            .unwrap()
            .contains("from your decisions in Actual AI: wrote 1, removed 1 rule files."));
        let record = load(&records, root);
        assert_eq!(record.connected_repo.as_ref().unwrap().org_id, ORG);

        // The second refresh skips the repository lookup and changes nothing.
        let second = refresh(root, &args(Some(&server)), TODAY, Some(&records));
        assert!(
            second.written.is_empty() && second.message.is_none(),
            "{second:?}"
        );
        assert_eq!(
            render(&second),
            ".actual/rules/ already matches your decisions in Actual AI.\n"
        );
        repos.assert();
        list.assert();
    }

    #[test]
    fn signed_in_refresh_keeps_the_rule_files_when_actual_ai_fails_or_is_slow() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let config = tempdir().unwrap();
        let _guards = sign_in(config.path());
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        let kept = write(
            root,
            ".actual/rules/kept.md",
            "---\nversion: 2\nmetadata:\n  actual:\n    adr_id: archived-1\n---\n# Kept\n",
        );
        let mut server = mockito::Server::new();
        server
            .mock("GET", "/v1/gcp/product/decisions")
            .match_query(Matcher::Any)
            .with_status(500)
            .create();
        let mut failing = args(Some(&server));
        failing.api.json = false;
        // `--repo` isn't a refresh flag, so the remote decides: there is none.
        let report = refresh(root, &failing, TODAY, None);
        assert!(report.kept);
        assert!(
            report.problems[0].contains("No connected repository"),
            "{report:?}"
        );
        assert!(report.problems[0].ends_with("so the rule files were left as they are."));
        let rendered = render(&report);
        assert!(rendered.starts_with("warning: "), "{rendered}");

        git_repo(root, Some("git@github.com:actual-software/actual-cli.git"));
        server
            .mock("GET", "/v1/connected-repos")
            .match_query(Matcher::Any)
            .with_status(200)
            .with_body(CONNECTED)
            .create();
        let report = refresh(root, &args(Some(&server)), TODAY, None);
        assert!(
            report.kept && report.problems[0].contains("HTTP 500"),
            "{report:?}"
        );
        assert!(kept.exists());

        // A server that answers after the time box.
        let mut slow = mockito::Server::new();
        slow.mock("GET", "/v1/connected-repos")
            .match_query(Matcher::Any)
            .with_status(200)
            .with_chunked_body(|w| {
                std::thread::sleep(Duration::from_millis(400));
                w.write_all(CONNECTED.as_bytes())
            })
            .create();
        let mut hurried = args(Some(&slow));
        hurried.timeout_ms = 50;
        let report = refresh(root, &hurried, TODAY, None);
        assert!(report.kept);
        assert_eq!(
            report.problems,
            ["Actual AI didn't answer within 50 ms, so the rule files were left as they are."]
        );
        assert!(kept.exists());
    }

    #[test]
    fn an_access_token_about_to_expire_is_renewed_apart_and_the_files_kept() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let config = tempdir().unwrap();
        let _guards = signed_out(config.path());
        store::save(&StoredCredentials {
            access_token: "tok".to_string(),
            refresh_token: "refresh".to_string(),
            token_type: "Bearer".to_string(),
            expires_at: Some(chrono::Utc::now() + chrono::Duration::seconds(30)),
            scope: None,
            organization_id: ORG.to_string(),
            member_id: "member".to_string(),
            email: None,
            subject: None,
            auth_url: None,
        })
        .unwrap();
        let tmp = tempdir().unwrap();
        let before = RENEWALS.with(std::cell::Cell::get);
        // No server: a read would fail with another problem.
        let report = refresh(tmp.path(), &args(None), TODAY, None);
        assert_eq!(RENEWALS.with(std::cell::Cell::get), before + 1);
        assert_eq!(report.mode, Mode::SignedIn);
        assert!(report.kept);
        assert!(
            report.problems[0].starts_with("Your sign-in is being renewed in the background"),
            "{report:?}"
        );
    }

    #[test]
    fn a_sign_in_that_cannot_be_read_leaves_the_rule_files() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let config = tempdir().unwrap();
        let _guards = signed_out(config.path());
        let creds = store::credentials_path().unwrap();
        std::fs::create_dir_all(creds.parent().unwrap()).unwrap();
        std::fs::write(&creds, "not yaml: [").unwrap();
        let tmp = tempdir().unwrap();
        write(
            tmp.path(),
            "docs/adr/a.md",
            &decision_file(ID, "accepted", "Use Postgres", "MUST: Use Postgres."),
        );
        let report = refresh(tmp.path(), &args(None), TODAY, None);
        assert_eq!(report.mode, Mode::SignedIn);
        assert!(report.kept && report.written.is_empty());
        assert!(report.problems[0].starts_with("Couldn't read your sign-in"));
    }

    #[test]
    fn exec_refresh_prints_json_or_text_and_never_fails() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let config = tempdir().unwrap();
        let _guards = signed_out(config.path());
        let tmp = tempdir().unwrap();
        let mut args = args(None);
        args.root = Some(tmp.path().to_path_buf());
        exec_refresh(&args).unwrap();
        args.api.json = false;
        exec_refresh(&args).unwrap();
        assert!(record_dir().unwrap().ends_with(DIR_NAME));
    }

    #[test]
    fn refresh_decision_writes_rewrites_and_removes_one_decisions_rule_file() {
        let tmp = tempdir().unwrap();
        let records = tempdir().unwrap();
        let root = tmp.path();
        let dir = Some(records.path());
        let doc = write(
            root,
            "docs/adr/a.md",
            &decision_file(ID, "accepted", "Use Postgres", "MUST: Use Postgres."),
        );
        // Another accepted decision waits for a hook: this refresh is for one.
        write(
            root,
            "docs/adr/b.md",
            &decision_file("id-b", "accepted", "Use Redis", "MUST: Use Redis."),
        );
        let written = refresh_decision(root, ID, TODAY, dir, true);
        assert_eq!(written.written.len(), 1, "{written:?}");
        std::fs::write(
            &doc,
            decision_file(ID, "accepted", "Use Postgres", "SHOULD: Use Postgres."),
        )
        .unwrap();
        assert_eq!(
            refresh_decision(root, ID, TODAY, dir, true).rewritten.len(),
            1
        );
        std::fs::remove_file(&doc).unwrap();
        assert_eq!(
            refresh_decision(root, ID, TODAY, dir, false).removed.len(),
            1
        );
        assert!(load(records.path(), root).rules.is_empty());
        assert_eq!(rule_files_by_id(root).len(), 0);
    }

    #[test]
    fn refresh_decision_leaves_the_rule_file_of_a_file_that_isnt_normalized() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        // The header names the decision, but its one policy has no level.
        write(
            root,
            "docs/adr/a.md",
            &decision_file(ID, "accepted", "Use Postgres", "Use it."),
        );
        let applied = refresh_decision(root, ID, TODAY, None, true);
        assert!(applied.written.is_empty(), "{applied:?}");
        assert!(
            applied.problems[0].starts_with("docs/adr/a.md isn't normalized ("),
            "{applied:?}"
        );
    }

    #[test]
    fn the_list_message_counts_its_files() {
        let file = |path: &str| Unnormalized {
            path: path.to_string(),
            adr_id: None,
            reasons: vec!["It has no ADR header.".to_string()],
        };
        let list = [file("docs/adr/a.md"), file("docs/adr/b.md")];
        let text = message(Mode::SignedOut, &Applied::default(), Some(&list)).unwrap();
        assert!(
            text.starts_with(
                "2 files under docs/adr/ aren't normalized, so they get no rule file:"
            ),
            "{text}"
        );
    }

    #[test]
    fn a_record_another_refresh_holds_is_left_as_it_is() {
        let records = tempdir().unwrap();
        let root = tempdir().unwrap();
        let held = brief_ledger::lock(records.path(), LOCK_WAIT).unwrap();
        assert!(!save(records.path(), root.path(), |record| {
            record.unnormalized = Some("x".to_string());
        }));
        drop(held);
        assert_eq!(load(records.path(), root.path()), Record::default());
    }
}
