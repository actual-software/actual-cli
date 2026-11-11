//! `actual adr`: write and check ADRs in this repository, and manage the
//! organization's decisions in Actual AI.
//!
//! `new` and `check` work on local files and need no sign-in; the rest call
//! the decision routes with the signed-in session's token.

mod cloud;
mod local;

use std::io::IsTerminal;
use std::path::Path;

use uuid::Uuid;

use crate::adr::context_file::{ContextFile, Policy};
use crate::api::adr_types::{LifecycleAction, SaveStatus};
use crate::api::{resolve_api_url, ActualApiClient};
use crate::auth::refresh::ensure_fresh;
use crate::auth::store;
use crate::cli::args::{AdrAction, AdrApiOptions, AdrArgs, AdrContentArgs};
use crate::cli::commands::advisor::{
    match_remote_to_repos, origin_remote_url, parse_git_remote_url, qualified_name,
    resolve_repo_name,
};
use crate::error::{sanitize_server_text, ActualError};
use crate::rules::types::RuleLevel;

pub fn exec(args: &AdrArgs) -> Result<(), ActualError> {
    match &args.action {
        AdrAction::New(a) => local::exec_new(a),
        AdrAction::Check(a) => local::exec_check(a),
        AdrAction::List(a) => cloud::exec_list(a),
        AdrAction::Show(a) => cloud::exec_show(a),
        AdrAction::Create(a) => cloud::exec_create(a),
        AdrAction::Update(a) => cloud::exec_update(a),
        AdrAction::Publish(a) => cloud::exec_lifecycle(a, LifecycleAction::Publish),
        AdrAction::Unpublish(a) => cloud::exec_lifecycle(a, LifecycleAction::Unpublish),
        AdrAction::Delete(a) => cloud::exec_lifecycle(a, LifecycleAction::Archive),
        AdrAction::Restore(a) => cloud::exec_lifecycle(a, LifecycleAction::Restore),
        AdrAction::Accept(a) => cloud::exec_lifecycle(a, LifecycleAction::Accept),
        AdrAction::Reject(a) => cloud::exec_reject(a),
    }
}

/// Whether a person is at the terminal, read from standard input. A coding
/// agent's shell tool, a script and CI all run without one.
fn interactive() -> bool {
    std::io::stdin().is_terminal()
}

/// The status a save uses: the flag, else accepted at a terminal and draft
/// with no person at one, so an agent run never publishes a governing
/// decision on its own.
fn save_status(flag: Option<SaveStatus>, interactive: bool) -> SaveStatus {
    flag.unwrap_or(if interactive {
        SaveStatus::Accepted
    } else {
        SaveStatus::Draft
    })
}

/// Today's date in UTC, as a Context File's `Date:` line carries it.
fn today() -> String {
    chrono::Utc::now().format("%Y-%m-%d").to_string()
}

/// A single-threaded runtime for the async client, as `advisor` uses.
fn runtime() -> Result<tokio::runtime::Runtime, ActualError> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| ActualError::InternalError(format!("failed to build tokio runtime: {e}")))
}

/// Pretty JSON for `--json`. The CLI's own types always serialize.
fn json_text<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|e| format!("{{\"error\": \"{e}\"}}"))
}

/// A signed-in client and the organization its token was issued for, which
/// every decision route requires.
struct Session {
    client: ActualApiClient,
    org_id: String,
}

/// The stored sign-in, renewed if it is about to expire.
async fn session(api: &AdrApiOptions) -> Result<Session, ActualError> {
    let creds = store::load()?.ok_or(ActualError::NotLoggedIn)?;
    let creds = ensure_fresh(creds).await?;
    let client = ActualApiClient::new(&resolve_api_url(api.api_url.as_deref()))?
        .with_bearer(&creds.access_token);
    Ok(Session {
        client,
        org_id: creds.organization_id,
    })
}

/// A connected repository: its id, and how to name it to a person.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Repo {
    id: String,
    label: String,
}

/// The repository a command acts on, or why none could be picked.
#[derive(Debug, Clone, PartialEq, Eq)]
enum RepoPick {
    Chosen(Repo),
    Unresolved(String),
}

/// `--repo` (an id, a name or `owner/name`) when given, else the connected
/// repository the origin remote of `dir` names. A `--repo` value that
/// matches nothing is an error; a remote that matches no single repository
/// is `Unresolved`, with the reason.
async fn pick_repo(
    session: &Session,
    flag: Option<&str>,
    dir: &Path,
) -> Result<RepoPick, ActualError> {
    if let Some(value) = flag {
        if Uuid::parse_str(value).is_ok() {
            return Ok(RepoPick::Chosen(Repo {
                id: value.to_string(),
                label: value.to_string(),
            }));
        }
        let repos = session.client.list_connected_repos(&session.org_id).await?;
        let id = resolve_repo_name(value, &repos)?;
        let label = repos
            .iter()
            .find(|repo| repo.repo_unique_id == id)
            .map_or_else(|| id.clone(), qualified_name);
        return Ok(RepoPick::Chosen(Repo { id, label }));
    }
    let Some(url) = origin_remote_url(dir).await else {
        return Ok(RepoPick::Unresolved(
            "this directory has no origin remote".to_string(),
        ));
    };
    let Some(remote) = parse_git_remote_url(&url) else {
        return Ok(RepoPick::Unresolved(format!(
            "its origin remote ({}) doesn't name a repository",
            sanitize_server_text(&url)
        )));
    };
    let repos = session.client.list_connected_repos(&session.org_id).await?;
    Ok(match match_remote_to_repos(&remote, &repos).as_slice() {
        [one] => RepoPick::Chosen(Repo {
            id: one.repo_unique_id.clone(),
            label: qualified_name(one),
        }),
        [] => RepoPick::Unresolved(format!("{} is not a connected repository", remote.slug())),
        many => RepoPick::Unresolved(format!(
            "{} matches {} connected repositories",
            remote.slug(),
            many.len()
        )),
    })
}

/// A level as rule files spell it: `MUST NOT`, not `MUST_NOT`.
fn rule_spelling(level: RuleLevel) -> &'static str {
    match level {
        RuleLevel::Must => "MUST",
        RuleLevel::MustNot => "MUST NOT",
        RuleLevel::Should => "SHOULD",
        RuleLevel::ShouldNot => "SHOULD NOT",
        RuleLevel::May => "MAY",
    }
}

/// The reason a `--policy` value has no level.
fn missing_level(n: usize) -> String {
    format!(
        "Policy {n} doesn't start with a level: write it as `MUST: …`, `MUST NOT: …`, \
         `SHOULD: …`, `SHOULD NOT: …` or `MAY: …`."
    )
}

/// One `--policy` value, `LEVEL: statement`.
fn parse_policy(n: usize, text: &str) -> Result<Policy, String> {
    match RuleLevel::split_prefix(text) {
        Some((level, statement)) => Ok(Policy {
            level,
            statement: statement.to_string(),
        }),
        None => Err(missing_level(n)),
    }
}

/// A decision built from flags: the title, the `--context` points and the
/// `--policy` values. Every policy without a level is reported at once.
fn content_from_flags(title: &str, content: &AdrContentArgs) -> Result<ContextFile, ActualError> {
    let mut policies = Vec::new();
    let mut reasons = Vec::new();
    for (i, text) in content.policies.iter().enumerate() {
        match parse_policy(i + 1, text) {
            Ok(policy) => policies.push(policy),
            Err(reason) => reasons.push(reason),
        }
    }
    if !reasons.is_empty() {
        return Err(ActualError::AdrInvalid { reasons });
    }
    Ok(ContextFile {
        title: title.to_string(),
        deciders: content.deciders.clone().unwrap_or_default(),
        context: content.context.clone(),
        policies,
        ..ContextFile::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::store::StoredCredentials;
    use crate::testutil::{EnvGuard, ENV_MUTEX};
    use tempfile::tempdir;

    pub(super) const ORG: &str = "11111111-1111-4111-8111-111111111111";
    pub(super) const REPO: &str = "22222222-2222-4222-8222-222222222222";

    /// A signed-in session whose token expires in an hour, saved under a
    /// temporary config directory the guards point at.
    pub(super) fn sign_in(dir: &Path) -> (EnvGuard, EnvGuard) {
        let g1 = EnvGuard::remove("ACTUAL_CONFIG");
        let g2 = EnvGuard::set("ACTUAL_CONFIG_DIR", dir.to_str().unwrap());
        store::save(&StoredCredentials {
            access_token: "tok".to_string(),
            refresh_token: String::new(),
            token_type: "Bearer".to_string(),
            expires_at: Some(chrono::Utc::now() + chrono::Duration::hours(1)),
            scope: Some("openid adr:query adr:review".to_string()),
            organization_id: ORG.to_string(),
            member_id: "member".to_string(),
            email: None,
            subject: None,
            auth_url: None,
        })
        .unwrap();
        (g1, g2)
    }

    pub(super) fn api(server: &mockito::Server) -> AdrApiOptions {
        AdrApiOptions {
            api_url: Some(server.url()),
            json: false,
        }
    }

    pub(super) const CONNECTED: &str = r#"{"repositories":[
        {"repo_unique_id":"22222222-2222-4222-8222-222222222222","name":"actual-cli","external_owner":"actual-software","url":"https://github.com/actual-software/actual-cli"},
        {"repo_unique_id":"44444444-4444-4444-8444-444444444444","name":"web","external_owner":"actual-software","url":""},
        {"repo_unique_id":"55555555-5555-4555-8555-555555555555","name":"web","external_owner":"fork-owner","url":""}
    ]}"#;

    /// A git checkout in `dir` whose origin remote is `url`.
    pub(super) fn git_repo(dir: &Path, url: Option<&str>) {
        let run = |args: &[&str]| {
            std::process::Command::new("git")
                .args(args)
                .current_dir(dir)
                .output()
                .unwrap();
        };
        run(&["init", "-q"]);
        if let Some(url) = url {
            run(&["remote", "add", "origin", url]);
        }
    }

    #[test]
    fn save_status_defaults_on_who_is_at_the_terminal() {
        assert_eq!(save_status(None, true), SaveStatus::Accepted);
        assert_eq!(save_status(None, false), SaveStatus::Draft);
        assert_eq!(
            save_status(Some(SaveStatus::Accepted), false),
            SaveStatus::Accepted
        );
        assert_eq!(
            save_status(Some(SaveStatus::Draft), true),
            SaveStatus::Draft
        );
    }

    #[test]
    fn today_is_a_date() {
        let date = today();
        assert_eq!(date.len(), 10);
        assert!(chrono::NaiveDate::parse_from_str(&date, "%Y-%m-%d").is_ok());
        // The tests run without a terminal on standard input.
        let _ = interactive();
    }

    #[test]
    fn json_text_is_pretty() {
        assert_eq!(json_text(&serde_json::json!({"a": 1})), "{\n  \"a\": 1\n}");
    }

    #[test]
    fn rule_spelling_uses_spaces() {
        let spelled: Vec<_> = [
            RuleLevel::Must,
            RuleLevel::MustNot,
            RuleLevel::Should,
            RuleLevel::ShouldNot,
            RuleLevel::May,
        ]
        .into_iter()
        .map(rule_spelling)
        .collect();
        assert_eq!(spelled, ["MUST", "MUST NOT", "SHOULD", "SHOULD NOT", "MAY"]);
    }

    #[test]
    fn content_from_flags_reads_levels_and_reports_every_bad_policy() {
        let content = AdrContentArgs {
            context: vec!["Why.".to_string()],
            policies: vec![
                "MUST NOT: Skip the check.".to_string(),
                "should prefer Postgres".to_string(),
            ],
            deciders: Some("Ana".to_string()),
        };
        let doc = content_from_flags("Use Postgres", &content).unwrap();
        assert_eq!(doc.title, "Use Postgres");
        assert_eq!(doc.deciders, "Ana");
        assert_eq!(doc.policies[0].level, RuleLevel::MustNot);
        assert_eq!(doc.policies[0].statement, "Skip the check.");
        assert_eq!(doc.policies[1].level, RuleLevel::Should);

        let bad = AdrContentArgs {
            policies: vec![
                "Use it.".to_string(),
                "MUST: ok".to_string(),
                "Never.".to_string(),
            ],
            ..AdrContentArgs::default()
        };
        match content_from_flags("T", &bad) {
            Err(ActualError::AdrInvalid { reasons }) => {
                assert_eq!(reasons.len(), 2);
                assert!(reasons[0].starts_with("Policy 1 doesn't start with a level"));
                assert!(reasons[1].starts_with("Policy 3 "));
            }
            other => panic!("expected AdrInvalid, got {other:?}"),
        }
    }

    #[test]
    fn exec_dispatches_every_action() {
        use clap::Parser;
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::remove("ACTUAL_CONFIG");
        let tmp = tempdir().unwrap();
        let _g2 = EnvGuard::set("ACTUAL_CONFIG_DIR", tmp.path().to_str().unwrap());
        // Signed out, so every cloud action stops before any request.
        let cloud = [
            vec!["list"],
            vec!["show", "x"],
            vec!["create", "--title", "T"],
            vec!["update", "x", "--title", "T"],
            vec!["publish", "x"],
            vec!["unpublish", "x"],
            vec!["delete", "x"],
            vec!["restore", "x"],
            vec!["accept", "x"],
            vec!["reject", "x", "--reason", "r"],
        ];
        for args in cloud {
            let mut argv = vec!["actual", "adr"];
            argv.extend(args.iter().copied());
            let cli = crate::Cli::parse_from(argv.clone());
            let crate::Command::Adr(adr) = cli.command else {
                panic!("not an adr command: {argv:?}");
            };
            let err = exec(&adr).unwrap_err();
            assert!(
                matches!(
                    err,
                    ActualError::NotLoggedIn | ActualError::AdrInvalid { .. }
                ),
                "{argv:?}: {err:?}"
            );
        }
        let root = tmp.path().to_str().unwrap();
        for args in [
            vec!["new", "T", "--root", root],
            vec!["check", "--root", root, "--no-cloud"],
        ] {
            let mut argv = vec!["actual", "adr"];
            argv.extend(args.iter().copied());
            let cli = crate::Cli::parse_from(argv);
            let crate::Command::Adr(adr) = cli.command else {
                panic!("not an adr command");
            };
            // `new` with no context or policy fails the save check; `check`
            // of an empty repository passes.
            let _ = exec(&adr);
        }
    }

    #[test]
    fn pick_repo_prefers_the_flag_then_the_origin_remote() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempdir().unwrap();
        let _guards = sign_in(tmp.path());
        let mut server = mockito::Server::new();
        let _repos = server
            .mock("GET", "/v1/connected-repos")
            .match_query(mockito::Matcher::Any)
            .with_status(200)
            .with_body(CONNECTED)
            .create();
        let rt = runtime().unwrap();
        let session = rt.block_on(session(&api(&server))).unwrap();
        assert_eq!(session.org_id, ORG);

        let repo_dir = tempdir().unwrap();
        let pick = |flag: Option<&str>, dir: &Path| rt.block_on(pick_repo(&session, flag, dir));

        // An id is taken as given, with no lookup.
        assert_eq!(
            pick(Some(REPO), repo_dir.path()).unwrap(),
            RepoPick::Chosen(Repo {
                id: REPO.to_string(),
                label: REPO.to_string()
            })
        );
        // A name resolves through the connected repositories.
        assert_eq!(
            pick(Some("actual-cli"), repo_dir.path()).unwrap(),
            RepoPick::Chosen(Repo {
                id: REPO.to_string(),
                label: "actual-software/actual-cli".to_string()
            })
        );
        assert!(matches!(
            pick(Some("nope"), repo_dir.path()),
            Err(ActualError::RepoNotFound(_))
        ));

        // No flag: the origin remote decides.
        assert_eq!(
            pick(None, repo_dir.path()).unwrap(),
            RepoPick::Unresolved("this directory has no origin remote".to_string())
        );
        git_repo(
            repo_dir.path(),
            Some("git@github.com:actual-software/actual-cli.git"),
        );
        assert_eq!(
            pick(None, repo_dir.path()).unwrap(),
            RepoPick::Chosen(Repo {
                id: REPO.to_string(),
                label: "actual-software/actual-cli".to_string()
            })
        );
        let other = tempdir().unwrap();
        git_repo(
            other.path(),
            Some("https://github.com/someone/elsewhere.git"),
        );
        assert_eq!(
            pick(None, other.path()).unwrap(),
            RepoPick::Unresolved("someone/elsewhere is not a connected repository".to_string())
        );
        let shared = tempdir().unwrap();
        git_repo(shared.path(), Some("https://github.com/third/web.git"));
        assert_eq!(
            pick(None, shared.path()).unwrap(),
            RepoPick::Unresolved("third/web matches 2 connected repositories".to_string())
        );
        let odd = tempdir().unwrap();
        git_repo(odd.path(), Some("odd"));
        assert_eq!(
            pick(None, odd.path()).unwrap(),
            RepoPick::Unresolved("its origin remote (odd) doesn't name a repository".to_string())
        );
    }

    #[test]
    fn pick_repo_labels_an_unlisted_id_by_itself() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempdir().unwrap();
        let _guards = sign_in(tmp.path());
        let mut server = mockito::Server::new();
        let _repos = server
            .mock("GET", "/v1/connected-repos")
            .match_query(mockito::Matcher::Any)
            .with_status(200)
            .with_body(
                r#"{"repositories":[{"repo_unique_id":"r-1","name":"solo","external_owner":"o"}]}"#,
            )
            .create();
        let rt = runtime().unwrap();
        let session = rt.block_on(session(&api(&server))).unwrap();
        let picked = rt
            .block_on(pick_repo(&session, Some("solo"), tmp.path()))
            .unwrap();
        assert_eq!(
            picked,
            RepoPick::Chosen(Repo {
                id: "r-1".to_string(),
                label: "o/solo".to_string()
            })
        );
    }

    #[test]
    fn session_needs_a_sign_in() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::remove("ACTUAL_CONFIG");
        let tmp = tempdir().unwrap();
        let _g2 = EnvGuard::set("ACTUAL_CONFIG_DIR", tmp.path().to_str().unwrap());
        let rt = runtime().unwrap();
        assert!(matches!(
            rt.block_on(session(&AdrApiOptions::default())),
            Err(ActualError::NotLoggedIn)
        ));
    }
}
