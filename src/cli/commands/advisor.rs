//! `actual advisor <query>` — ask the Advisor org-scoped architecture questions.
//!
//! Starts an async advisor job, polls to completion (honoring the server's
//! `Retry-After` and retrying transient network/5xx errors up to a ~5-minute
//! cap), and renders the answer. Uses the platform token from `actual login`
//! as the bearer.

use std::path::Path;
use std::time::{Duration, Instant};

use chrono::{Duration as ChronoDuration, Utc};

use uuid::Uuid;

use crate::api::types::{
    AdvisorJobStatus, AdvisorOutput, AdvisorPoll, AdvisorQueryRequest, AdvisorSink, AdvisorSurface,
    ConnectedRepository,
};
use crate::api::{ActualApiClient, DEFAULT_API_URL};
use crate::auth::oauth;
use crate::auth::store::{self, StoredCredentials};
use crate::cli::args::AdvisorArgs;
use crate::cli::ui::theme;
use crate::config::sticky;
use crate::config::types::StickyScope;
use crate::error::ActualError;
use sha2::{Digest, Sha256};

/// Hard wall-clock cap on polling before giving up, matching the browser
/// reference. A true deadline (not an attempt count) is what actually bounds
/// total time once the per-poll `Retry-After` back-off varies.
const HARD_TIMEOUT: Duration = Duration::from_secs(5 * 60);
/// Default delay between polls when the server provides no `Retry-After`.
const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(2);
/// Upper bound on a server-supplied `Retry-After`. The status handler caps its
/// own back-off at 15s, so a larger (or misbehaving) value must not let a single
/// poll stall the whole query.
const MAX_RETRY_AFTER: Duration = Duration::from_secs(15);
/// Wall-clock cap on the `git remote get-url origin` lookup used for repo
/// auto-detection, mirroring the repo-key helper's bound so a wedged git can't
/// stall the command.
const GIT_REMOTE_TIMEOUT: Duration = Duration::from_secs(5);

pub fn exec(args: &AdvisorArgs) -> Result<(), ActualError> {
    // The working directory drives git-remote auto-detection when `--repo` is
    // omitted, and keys the remembered scope. Resolving it here (rather than
    // inside `run`) keeps `run` testable with an explicit directory.
    let repo_dir = std::env::current_dir().ok();
    build_runtime()?.block_on(run(
        args,
        repo_dir.as_deref(),
        HARD_TIMEOUT,
        DEFAULT_POLL_INTERVAL,
    ))
}

/// Single-threaded tokio runtime so the sync CLI dispatch path can drive the
/// async query flow (mirrors `commands::login`).
fn build_runtime() -> Result<tokio::runtime::Runtime, ActualError> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| ActualError::InternalError(format!("failed to build tokio runtime: {e}")))
}

/// Resolve the Advisor API base URL: the `--api-url` flag wins, then the
/// `ACTUAL_API_URL` environment variable, else the api-service default. This
/// mirrors how `login` honors `ACTUAL_AUTH_URL`, so a single export steers both
/// the auth and advisor halves against a local stack. An empty env var is
/// treated as unset.
fn resolve_api_url(flag: Option<&str>) -> String {
    flag.map(|s| s.to_string())
        .or_else(|| {
            std::env::var("ACTUAL_API_URL")
                .ok()
                .filter(|s| !s.is_empty())
        })
        .unwrap_or_else(|| DEFAULT_API_URL.to_string())
}

/// A `--repo` value resolved to an id, plus the repo's `owner/name` when it is
/// known. A UUID passed directly resolves without a name lookup, so its
/// `qualified_name` is `None`.
struct ResolvedRepo {
    repo_unique_id: String,
    qualified_name: Option<String>,
}

/// Resolve a `--repo` value to a [`ResolvedRepo`]. A value that parses as a UUID
/// is already an id and is returned as-is (no lookup, backward compatible); any
/// other value is treated as a repository name and resolved against the org's
/// connected repositories via `GET /v1/connected-repos`, which also yields the
/// repo's `owner/name` for display and for the remembered scope.
async fn resolve_repo(
    value: &str,
    client: &ActualApiClient,
    org_id: &str,
) -> Result<ResolvedRepo, ActualError> {
    if Uuid::parse_str(value).is_ok() {
        return Ok(ResolvedRepo {
            repo_unique_id: value.to_string(),
            qualified_name: None,
        });
    }
    let repos = client.list_connected_repos(org_id).await?;
    let repo_unique_id = resolve_repo_name(value, &repos)?;
    let qualified_name = repos
        .iter()
        .find(|r| r.repo_unique_id == repo_unique_id)
        .map(qualified_name);
    Ok(ResolvedRepo {
        repo_unique_id,
        qualified_name,
    })
}

/// Match a repository name against the org's connected repos. A bare `name`
/// matches the `name` field; an `owner/name` form additionally constrains the
/// owner, disambiguating a name shared across owners. Zero matches — or a bare
/// name shared across owners — is a `RepoNotFound` error whose message lists the
/// choices.
fn resolve_repo_name(value: &str, repos: &[ConnectedRepository]) -> Result<String, ActualError> {
    let matches: Vec<&ConnectedRepository> = match value.split_once('/') {
        Some((owner, name)) => repos
            .iter()
            .filter(|r| {
                r.external_owner.eq_ignore_ascii_case(owner) && r.name.eq_ignore_ascii_case(name)
            })
            .collect(),
        None => repos
            .iter()
            .filter(|r| r.name.eq_ignore_ascii_case(value))
            .collect(),
    };
    match matches.as_slice() {
        [single] => Ok(single.repo_unique_id.clone()),
        [] => Err(ActualError::RepoNotFound(not_found_message(value, repos))),
        multiple => Err(ActualError::RepoNotFound(ambiguous_message(
            value, multiple,
        ))),
    }
}

/// A connected repo rendered as `owner/name` for user-facing lists.
fn qualified_name(repo: &ConnectedRepository) -> String {
    format!("{}/{}", repo.external_owner, repo.name)
}

/// Build the "no match" error message, listing every connected repository (or
/// noting when the organization has none connected).
fn not_found_message(value: &str, repos: &[ConnectedRepository]) -> String {
    if repos.is_empty() {
        return format!(
            "No repository named '{value}': this organization has no connected repositories."
        );
    }
    let mut msg = format!("No connected repository matches '{value}'. Connected repositories:");
    for repo in repos {
        msg.push_str("\n  • ");
        msg.push_str(&qualified_name(repo));
    }
    msg
}

/// Build the "ambiguous bare name" error message, listing the owner-qualified
/// candidates so the caller can pick one.
fn ambiguous_message(value: &str, matches: &[&ConnectedRepository]) -> String {
    let mut msg =
        format!("'{value}' matches multiple connected repositories; qualify it as owner/name:");
    for repo in matches {
        msg.push_str("\n  • ");
        msg.push_str(&qualified_name(repo));
    }
    msg
}

/// A git remote parsed into the repository's owner and name — the
/// `actual-software` / `actual-cli` of `git@github.com:actual-software/actual-cli.git`.
#[derive(Debug, PartialEq, Eq)]
struct RepoRemote {
    owner: String,
    name: String,
}

impl RepoRemote {
    /// The `owner/name` form used in user-facing messages.
    fn slug(&self) -> String {
        format!("{}/{}", self.owner, self.name)
    }
}

/// Parse a git remote URL into its owner and repository name, covering the
/// common remote forms: SSH (`git@github.com:owner/name.git`), HTTPS
/// (`https://github.com/owner/name(.git)`), and the `ssh://` URL variant. The
/// last two path segments are taken as owner and name, so a scheme, an optional
/// `user@`, and a `host:port` prefix are all tolerated. Returns `None` when the
/// URL does not yield an owner/name pair.
fn parse_git_remote_url(url: &str) -> Option<RepoRemote> {
    let trimmed = url.trim().trim_end_matches('/');
    let without_git = trimmed.strip_suffix(".git").unwrap_or(trimmed);
    // Split on both the path separator and the scp-style host separator, dropping
    // empty segments (the `//` after a scheme, a leading separator).
    let segments: Vec<&str> = without_git
        .split(['/', ':'])
        .filter(|s| !s.is_empty())
        .collect();
    if segments.len() < 2 {
        return None;
    }
    Some(RepoRemote {
        owner: segments[segments.len() - 2].to_string(),
        name: segments[segments.len() - 1].to_string(),
    })
}

/// Match a parsed origin remote against the org's connected repos using the
/// exact `owner`+`name` pair (case-insensitive, as GitHub owners and repo names
/// are). A name alone is not authoritative: it may belong to an unrelated repo
/// and must fall back to org-level querying instead of silently mis-scoping.
fn match_remote_to_repos<'a>(
    remote: &RepoRemote,
    repos: &'a [ConnectedRepository],
) -> Vec<&'a ConnectedRepository> {
    repos
        .iter()
        .filter(|r| {
            r.external_owner.eq_ignore_ascii_case(&remote.owner)
                && r.name.eq_ignore_ascii_case(&remote.name)
        })
        .collect()
}

/// The first 8 characters of a repo id, for a compact user-facing display.
fn short_id(id: &str) -> String {
    id.chars().take(8).collect()
}

/// Render a repo scope for a user-facing line: `owner/name (shortid)` when the
/// name is known, else `repo shortid` (a scope pinned by bare UUID has no name).
fn format_repo_scope(qualified_name: Option<&str>, id: &str) -> String {
    match qualified_name {
        Some(name) => format!("{} ({})", name, short_id(id)),
        None => format!("repo {}", short_id(id)),
    }
}

/// Best-effort auto-detection of the connected repo for the working tree at
/// `dir`. Resolves the `origin` remote, matches it against the org's connected
/// repositories, and returns the `repo_unique_id` when exactly one matches
/// (printing the scoped repo to stderr). Returns `None` — an org-level query —
/// when there is no remote, nothing matches, the match is ambiguous, or the
/// lookup fails; a short note on stderr explains the fallback where it helps.
async fn auto_detect_repo(dir: &Path, client: &ActualApiClient, org_id: &str) -> Option<String> {
    let url = crate::git::origin_remote_url(dir, GIT_REMOTE_TIMEOUT).await?;
    let remote = parse_git_remote_url(&url)?;
    let repos = match client.list_connected_repos(org_id).await {
        Ok(repos) => repos,
        // A speculative lookup failure (auth, network) must not fail the command;
        // the org-level query below surfaces any real error with better guidance.
        Err(_) => return None,
    };
    match match_remote_to_repos(&remote, &repos).as_slice() {
        [single] => {
            eprintln!(
                "{} scoped to {} ({})",
                theme::hint("advisor"),
                qualified_name(single),
                short_id(&single.repo_unique_id)
            );
            Some(single.repo_unique_id.clone())
        }
        [] => {
            eprintln!(
                "{} {} is not a connected repository; querying at org level",
                theme::hint("advisor"),
                remote.slug()
            );
            None
        }
        many => {
            eprintln!(
                "{} origin remote {} matches multiple connected repositories; \
                 pass --repo owner/name to scope one. Querying at org level.",
                theme::hint("advisor"),
                remote.slug()
            );
            for repo in many {
                eprintln!("    • {}", qualified_name(repo));
            }
            None
        }
    }
}

fn enrich_org_mismatch(
    err: ActualError,
    session_org: &str,
    target_org: &str,
    explicit_org: bool,
) -> ActualError {
    match err {
        ActualError::OrgMismatch { .. } => {
            let (message, hint) = org_mismatch_message(session_org, target_org, explicit_org);
            ActualError::OrgMismatch { message, hint }
        }
        other => other,
    }
}

fn org_mismatch_message(
    session_org: &str,
    target_org: &str,
    explicit_org: bool,
) -> (String, String) {
    if explicit_org && target_org != session_org {
        (
            format!(
                "Advisor request denied (HTTP 403): this session is scoped to organization \
                 {session_org}, but you requested organization {target_org}."
            ),
            format!("actual login --org {target_org}  (or drop --org to query {session_org})"),
        )
    } else {
        (
            format!(
                "Advisor request denied (HTTP 403): this session (organization {session_org}) \
                 was rejected as cross-organization, or your token carries no usable \
                 organization."
            ),
            "actual login  (optionally with --org <org-id>) to refresh your session".to_string(),
        )
    }
}

/// If the stored access token is expired (or about to be), refresh it with the
/// rotation primitive and re-persist. A refresh failure surfaces as
/// `NotLoggedIn` — the user must re-run `actual login`.
async fn ensure_fresh(creds: StoredCredentials) -> Result<StoredCredentials, ActualError> {
    if creds.refresh_token.is_empty() {
        return Ok(creds);
    }
    if creds.expires_within(Utc::now(), ChronoDuration::seconds(60)) {
        let refreshed = oauth::refresh(&creds)
            .await
            .map_err(|_| ActualError::NotLoggedIn)?;
        store::save(&refreshed)?;
        return Ok(refreshed);
    }
    Ok(creds)
}

/// Terminal outcome of an advisor job.
enum Outcome {
    Succeeded(Box<AdvisorOutput>),
    Failed(Option<String>),
}

/// Compute the per-repo key that indexes the remembered scope, or `None` when
/// there is no working directory. The key is `sha256(origin_url)`, falling back
/// to `sha256(path)` when the tree has no `origin` remote — byte-for-byte the
/// same scheme as `sync::cache::compute_repo_key`, so a repo has one stable key
/// across every per-repo config feature.
async fn sticky_key(repo_dir: Option<&Path>) -> Option<String> {
    let dir = repo_dir?;
    let input = crate::git::origin_remote_url(dir, GIT_REMOTE_TIMEOUT)
        .await
        .unwrap_or_else(|| dir.to_string_lossy().to_string());
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    Some(format!("{:x}", hasher.finalize()))
}

/// Auto-detect the repo from the working tree's origin remote when a directory
/// is known, else run at org level. Best-effort: an unresolved detection (no
/// repo, no match, ambiguous, or an API error) falls back to `None`.
async fn autodetect(
    repo_dir: Option<&Path>,
    client: &ActualApiClient,
    org_id: &str,
) -> Option<String> {
    match repo_dir {
        Some(dir) => auto_detect_repo(dir, client, org_id).await,
        None => None,
    }
}

/// Announce and apply a remembered scope. A repo pin scopes the query to that
/// repo; an org-level pin runs the query org-wide.
fn apply_remembered(scope: &StickyScope) -> Option<String> {
    match &scope.repo_unique_id {
        Some(id) => {
            eprintln!(
                "{} using remembered scope {}",
                theme::hint("advisor"),
                format_repo_scope(scope.qualified_name.as_deref(), id)
            );
            Some(id.clone())
        }
        None => {
            eprintln!(
                "{} using remembered org-level scope",
                theme::hint("advisor")
            );
            None
        }
    }
}

/// Load the config, apply `mutate`, and save it back (best-effort persistence of
/// a scope change).
fn update_config(mutate: impl FnOnce(&mut crate::config::Config)) -> Result<(), ActualError> {
    let mut config = crate::config::paths::load()?;
    mutate(&mut config);
    crate::config::paths::save(&config)
}

/// Decide the repo id to scope the query on, applying and persisting any scope
/// change requested via `--repo`. Precedence: an explicit `--repo` value (a repo
/// or a reserved keyword) > an explicit `--org` (a one-shot org-level query) > a
/// scope remembered for this repo > git-remote auto-detection > an org-level
/// fallback. `None` means an org-level query.
async fn determine_scope(
    args: &AdvisorArgs,
    repo_key: Option<&str>,
    repo_dir: Option<&Path>,
    client: &ActualApiClient,
    org_id: &str,
    session_org: &str,
    explicit_org: bool,
) -> Result<Option<String>, ActualError> {
    match args.repo.as_deref() {
        // Reserved keyword: pin the scope to org level (opt out of repo scoping).
        Some("none") => {
            if let Some(key) = repo_key {
                update_config(|c| sticky::set_scope(c, key, StickyScope::org_level()))?;
            }
            eprintln!("{} scope pinned to org level", theme::hint("advisor"));
            Ok(None)
        }
        // Reserved keyword: forget any pin and re-auto-detect for this call.
        Some("auto") => {
            if let Some(key) = repo_key {
                update_config(|c| sticky::clear_scope(c, key))?;
            }
            eprintln!("{} scope reset to auto-detect", theme::hint("advisor"));
            Ok(autodetect(repo_dir, client, org_id).await)
        }
        // Explicit repo by name or id: resolve, announce, and remember it. A 403
        // during a name lookup gets the same cross-org guidance as the query.
        Some(value) => {
            let resolved = resolve_repo(value, client, org_id)
                .await
                .map_err(|e| enrich_org_mismatch(e, session_org, org_id, explicit_org))?;
            if let Some(key) = repo_key {
                let scope =
                    StickyScope::repo(&resolved.repo_unique_id, resolved.qualified_name.clone());
                update_config(|c| sticky::set_scope(c, key, scope))?;
            }
            eprintln!(
                "{} scoped to {}",
                theme::hint("advisor"),
                format_repo_scope(resolved.qualified_name.as_deref(), &resolved.repo_unique_id)
            );
            Ok(Some(resolved.repo_unique_id))
        }
        None => {
            // An explicit --org is a one-shot org-level override; it does not
            // change a remembered pin.
            if explicit_org {
                eprintln!("{} querying at org level", theme::hint("advisor"));
                return Ok(None);
            }
            // A remembered pin wins over auto-detection.
            if let Some(key) = repo_key {
                if let Some(scope) = sticky::get_scope(&crate::config::paths::load()?, key) {
                    return Ok(apply_remembered(&scope));
                }
            }
            // Nothing pinned: auto-detect (best-effort), with an org fallback.
            Ok(autodetect(repo_dir, client, org_id).await)
        }
    }
}

/// Print the remembered advisor scope for this repo, or explain the default
/// (auto-detect) behavior when nothing is pinned. Reads local config only — no
/// token refresh, no API call.
fn show_scope(repo_key: Option<&str>) -> Result<(), ActualError> {
    let scope = match repo_key {
        Some(key) => sticky::get_scope(&crate::config::paths::load()?, key),
        None => None,
    };
    match scope {
        Some(s) => match &s.repo_unique_id {
            Some(id) => println!(
                "Active advisor scope: {} (pinned)",
                format_repo_scope(s.qualified_name.as_deref(), id)
            ),
            None => println!("Active advisor scope: org level (pinned)"),
        },
        None => println!(
            "Active advisor scope: not pinned — auto-detected from the git remote, \
             or org level if no connected repo matches"
        ),
    }
    Ok(())
}

async fn run(
    args: &AdvisorArgs,
    repo_dir: Option<&Path>,
    deadline: Duration,
    poll_interval: Duration,
) -> Result<(), ActualError> {
    let creds = store::load()?.ok_or(ActualError::NotLoggedIn)?;

    // Key the remembered scope by the working tree's origin remote (falling back
    // to its path). This is the same SHA-256-of-origin-URL that `rejected_adrs`
    // keys on (see `sync::cache::compute_repo_key`), computed here via the async
    // remote lookup so it stays inside this runtime.
    let repo_key = sticky_key(repo_dir).await;

    // `--show-scope` inspects the remembered scope from local config and exits
    // without contacting the API (no token refresh, no query).
    if args.show_scope {
        return show_scope(repo_key.as_deref());
    }

    let creds = ensure_fresh(creds).await?;
    let org_id = args
        .org
        .clone()
        .unwrap_or_else(|| creds.organization_id.clone());
    // Captured for the cross-org 403 message: the session's own org, and
    // whether the caller targeted a different org via an explicit `--org`.
    let session_org = creds.organization_id.clone();
    let explicit_org = args.org.is_some();
    let base_url = resolve_api_url(args.api_url.as_deref());
    let client = ActualApiClient::new(&base_url)?.with_bearer(&creds.access_token);

    let repo_unique_id = determine_scope(
        args,
        repo_key.as_deref(),
        repo_dir,
        &client,
        &org_id,
        &session_org,
        explicit_org,
    )
    .await?;

    // A scope-management-only invocation (a `--repo` value with no question) has
    // applied and announced the new scope; there is nothing to query. Clap
    // guarantees a question is present unless `--show-scope`/`--repo` was given.
    let query = match args.query.as_deref() {
        Some(q) => q,
        None => return Ok(()),
    };

    let request = AdvisorQueryRequest::new(
        org_id.clone(),
        repo_unique_id,
        query.to_string(),
        AdvisorSurface::cli(),
        AdvisorSink::None,
        None,
    );

    let started = client
        .start_advisor_query(&request)
        .await
        .map_err(|e| enrich_org_mismatch(e, &session_org, &org_id, explicit_org))?;
    eprintln!("{} thinking…", theme::hint("advisor"));
    let outcome = poll_to_completion(&client, &started.query_id, deadline, poll_interval)
        .await
        .map_err(|e| enrich_org_mismatch(e, &session_org, &org_id, explicit_org))?;

    match outcome {
        Outcome::Succeeded(output) => {
            print_answer(&output);
            Ok(())
        }
        Outcome::Failed(error) => Err(ActualError::ApiError(format!(
            "Advisor query failed: {}",
            error.unwrap_or_else(|| "unknown error".to_string())
        ))),
    }
}

/// Poll the job until it reaches a terminal state, or the wall-clock `deadline`
/// elapses (a true time bound — an attempt count can't bound total time once the
/// server's `Retry-After` back-off varies).
async fn poll_to_completion(
    client: &ActualApiClient,
    query_id: &str,
    deadline: Duration,
    poll_interval: Duration,
) -> Result<Outcome, ActualError> {
    let start = Instant::now();
    while start.elapsed() < deadline {
        match client.poll_advisor_query(query_id, None).await? {
            AdvisorPoll::Update {
                status,
                retry_after,
                ..
            } => match status.status {
                AdvisorJobStatus::Succeeded => {
                    return Ok(match status.result {
                        Some(output) => Outcome::Succeeded(Box::new(output)),
                        None => Outcome::Failed(Some("advisor returned no result".to_string())),
                    });
                }
                AdvisorJobStatus::Failed => return Ok(Outcome::Failed(status.error)),
                AdvisorJobStatus::Pending | AdvisorJobStatus::Running => {
                    sleep_for(retry_after, poll_interval).await;
                }
            },
            AdvisorPoll::NotModified => sleep_for(None, poll_interval).await,
            // Transient infra 5xx — back off (honoring Retry-After) and re-poll.
            AdvisorPoll::Retry { retry_after } => sleep_for(retry_after, poll_interval).await,
        }
    }
    Err(ActualError::ApiError(
        "Advisor query did not reach a result in time.".to_string(),
    ))
}

/// The next-poll delay: the server's `Retry-After` seconds, or the default
/// interval, **clamped to `MAX_RETRY_AFTER`** so a large or misbehaving
/// `Retry-After` can't stall a single poll past the wall-clock deadline's intent.
fn next_delay(retry_after: Option<u64>, default: Duration) -> Duration {
    retry_after
        .map(Duration::from_secs)
        .unwrap_or(default)
        .min(MAX_RETRY_AFTER)
}

async fn sleep_for(retry_after: Option<u64>, default: Duration) {
    tokio::time::sleep(next_delay(retry_after, default)).await;
}

/// Render the advisor answer. **Never prints token material.**
fn print_answer(output: &AdvisorOutput) {
    println!("{}", output.summary);
    if !output.interpreter.related_adrs.is_empty() {
        println!("\n{}", theme::hint("Related ADRs:"));
        for adr in &output.interpreter.related_adrs {
            println!(
                "  • {} ({}, confidence {:.0}%)",
                adr.title,
                adr.scope,
                adr.confidence * 100.0
            );
            // Render the server-provided deep link (used verbatim) on its own
            // line; skip a null or empty url so the ADR still prints cleanly.
            if let Some(url) = adr.url.as_deref().filter(|u| !u.is_empty()) {
                println!("    {url}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::store::StoredCredentials;
    use crate::testutil::{EnvGuard, ENV_MUTEX};
    use tempfile::tempdir;

    const POLL_PATH: &str = "/v1/advisor/query/q1";
    const START_BODY: &str = r#"{"query_id":"q1","workflow_id":"wf","status":"pending"}"#;

    fn test_creds() -> StoredCredentials {
        StoredCredentials {
            access_token: "tok".to_string(),
            refresh_token: "r".to_string(),
            token_type: "Bearer".to_string(),
            expires_at: None,
            scope: None,
            organization_id: "11111111-1111-1111-1111-111111111111".to_string(),
            member_id: "m".to_string(),
            email: None,
            subject: None,
            auth_url: Some("http://localhost:4000".to_string()),
        }
    }

    fn succeeded_body(adrs_json: &str) -> String {
        format!(
            r#"{{"query_id":"q1","status":"succeeded","result":{{"summary":"Use the App Router.","interpreter":{{"summary":"i","related_adrs":[{adrs_json}]}}}},"error":null}}"#
        )
    }

    const ONE_ADR: &str = r#"{"id":"a1","name":"n","title":"Use the App Router","policy":"p","instructions":"i","scope":"frontend","relevance_reason":"r","confidence":0.92}"#;

    fn args(api_url: &str, org: Option<&str>) -> AdvisorArgs {
        AdvisorArgs {
            query: Some("why app router?".to_string()),
            api_url: Some(api_url.to_string()),
            org: org.map(|s| s.to_string()),
            repo: None,
            show_scope: false,
        }
    }

    #[test]
    fn test_resolve_api_url() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());

        // The --api-url flag wins even when ACTUAL_API_URL is set.
        let g = EnvGuard::set("ACTUAL_API_URL", "http://env:9999");
        assert_eq!(
            resolve_api_url(Some("http://localhost:3099")),
            "http://localhost:3099"
        );
        drop(g);

        // No flag → the ACTUAL_API_URL env var is used when present.
        let g = EnvGuard::set("ACTUAL_API_URL", "http://env:9999");
        assert_eq!(resolve_api_url(None), "http://env:9999");
        drop(g);

        // No flag, empty env var → treated as unset, falls back to the default.
        let g = EnvGuard::set("ACTUAL_API_URL", "");
        assert_eq!(resolve_api_url(None), DEFAULT_API_URL);
        drop(g);

        // No flag, env unset → the api-service default.
        let g = EnvGuard::remove("ACTUAL_API_URL");
        assert_eq!(resolve_api_url(None), DEFAULT_API_URL);
        drop(g);
    }

    #[test]
    fn test_print_answer_with_and_without_adrs() {
        let adr = |url: Option<&str>| crate::api::types::RelatedAdr {
            id: "a".to_string(),
            name: "n".to_string(),
            title: "T".to_string(),
            policy: "p".to_string(),
            instructions: "i".to_string(),
            scope: "s".to_string(),
            relevance_reason: "r".to_string(),
            confidence: 0.5,
            url: url.map(|u| u.to_string()),
        };
        // Cover all three url arms: a populated link renders, while a null or
        // an empty link is skipped without breaking the ADR line.
        let with = AdvisorOutput {
            summary: "S".to_string(),
            interpreter: crate::api::types::AdvisorInterpreter {
                summary: "i".to_string(),
                related_adrs: vec![
                    adr(Some(
                        "https://app.example.com/decisions/r1?tab=active&decision=abc1234",
                    )),
                    adr(None),
                    adr(Some("")),
                ],
            },
        };
        print_answer(&with);
        let without = AdvisorOutput {
            summary: "S".to_string(),
            interpreter: crate::api::types::AdvisorInterpreter {
                summary: "i".to_string(),
                related_adrs: vec![],
            },
        };
        print_answer(&without);
    }

    #[test]
    fn test_exec_not_logged_in() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::remove("ACTUAL_CONFIG");
        let tmp = tempdir().unwrap();
        let _g2 = EnvGuard::set("ACTUAL_CONFIG_DIR", tmp.path().to_str().unwrap());
        // exec() itself resolves the working directory before running.
        let err = exec(&args("http://127.0.0.1:1", None)).unwrap_err();
        assert!(matches!(err, ActualError::NotLoggedIn));
    }

    #[tokio::test]
    async fn test_run_not_logged_in() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::remove("ACTUAL_CONFIG");
        let tmp = tempdir().unwrap();
        let _g2 = EnvGuard::set("ACTUAL_CONFIG_DIR", tmp.path().to_str().unwrap());
        let err = run(
            &args("http://127.0.0.1:1", None),
            None,
            Duration::from_secs(60),
            Duration::ZERO,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, ActualError::NotLoggedIn));
    }

    #[tokio::test]
    async fn test_run_success_renders_related_adrs() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::remove("ACTUAL_CONFIG");
        let tmp = tempdir().unwrap();
        let _g2 = EnvGuard::set("ACTUAL_CONFIG_DIR", tmp.path().to_str().unwrap());
        store::save(&test_creds()).unwrap();

        let mut server = mockito::Server::new_async().await;
        let s = server
            .mock("POST", "/v1/advisor/query")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(START_BODY)
            .create_async()
            .await;
        let p = server
            .mock("GET", POLL_PATH)
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(succeeded_body(ONE_ADR))
            .create_async()
            .await;

        // org omitted → uses the signed-in org from creds.
        run(
            &args(&server.url(), None),
            None,
            Duration::from_secs(60),
            Duration::ZERO,
        )
        .await
        .unwrap();
        s.assert_async().await;
        p.assert_async().await;
    }

    #[tokio::test]
    async fn test_run_success_no_related_adrs() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::remove("ACTUAL_CONFIG");
        let tmp = tempdir().unwrap();
        let _g2 = EnvGuard::set("ACTUAL_CONFIG_DIR", tmp.path().to_str().unwrap());
        store::save(&test_creds()).unwrap();

        let mut server = mockito::Server::new_async().await;
        let _s = server
            .mock("POST", "/v1/advisor/query")
            .with_body(START_BODY)
            .with_header("content-type", "application/json")
            .create_async()
            .await;
        let _p = server
            .mock("GET", POLL_PATH)
            .with_body(succeeded_body(""))
            .with_header("content-type", "application/json")
            .create_async()
            .await;
        run(
            &args(&server.url(), Some("00000000-0000-0000-0000-000000000000")),
            None,
            Duration::from_secs(60),
            Duration::ZERO,
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn test_run_failed_query() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::remove("ACTUAL_CONFIG");
        let tmp = tempdir().unwrap();
        let _g2 = EnvGuard::set("ACTUAL_CONFIG_DIR", tmp.path().to_str().unwrap());
        store::save(&test_creds()).unwrap();

        let mut server = mockito::Server::new_async().await;
        let _s = server
            .mock("POST", "/v1/advisor/query")
            .with_body(START_BODY)
            .with_header("content-type", "application/json")
            .create_async()
            .await;
        let _p = server
            .mock("GET", POLL_PATH)
            .with_body(
                r#"{"query_id":"q1","status":"failed","result":null,"error":"stream ended"}"#,
            )
            .with_header("content-type", "application/json")
            .create_async()
            .await;
        let err = run(
            &args(&server.url(), None),
            None,
            Duration::from_secs(60),
            Duration::ZERO,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, ActualError::ApiError(ref m) if m.contains("stream ended")));
    }

    #[tokio::test]
    async fn test_run_succeeded_without_result_is_failure() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::remove("ACTUAL_CONFIG");
        let tmp = tempdir().unwrap();
        let _g2 = EnvGuard::set("ACTUAL_CONFIG_DIR", tmp.path().to_str().unwrap());
        store::save(&test_creds()).unwrap();

        let mut server = mockito::Server::new_async().await;
        let _s = server
            .mock("POST", "/v1/advisor/query")
            .with_body(START_BODY)
            .with_header("content-type", "application/json")
            .create_async()
            .await;
        let _p = server
            .mock("GET", POLL_PATH)
            .with_body(r#"{"query_id":"q1","status":"succeeded","result":null,"error":null}"#)
            .with_header("content-type", "application/json")
            .create_async()
            .await;
        let err = run(
            &args(&server.url(), None),
            None,
            Duration::from_secs(60),
            Duration::ZERO,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, ActualError::ApiError(ref m) if m.contains("no result")));
    }

    #[tokio::test]
    async fn test_run_running_then_succeeded() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::remove("ACTUAL_CONFIG");
        let tmp = tempdir().unwrap();
        let _g2 = EnvGuard::set("ACTUAL_CONFIG_DIR", tmp.path().to_str().unwrap());
        store::save(&test_creds()).unwrap();

        let mut server = mockito::Server::new_async().await;
        let _s = server
            .mock("POST", "/v1/advisor/query")
            .with_body(START_BODY)
            .with_header("content-type", "application/json")
            .create_async()
            .await;
        // First poll: running (Retry-After: 0 → immediate). Second: succeeded.
        let _running = server
            .mock("GET", POLL_PATH)
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_header("retry-after", "0")
            .with_body(r#"{"query_id":"q1","status":"running","result":null,"error":null}"#)
            .expect(1)
            .create_async()
            .await;
        let _done = server
            .mock("GET", POLL_PATH)
            .with_body(succeeded_body(ONE_ADR))
            .with_header("content-type", "application/json")
            .create_async()
            .await;
        // org provided via --org (exercises the args.org branch).
        run(
            &args(&server.url(), Some("22222222-2222-2222-2222-222222222222")),
            None,
            Duration::from_secs(60),
            Duration::ZERO,
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn test_run_not_modified_then_succeeded() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::remove("ACTUAL_CONFIG");
        let tmp = tempdir().unwrap();
        let _g2 = EnvGuard::set("ACTUAL_CONFIG_DIR", tmp.path().to_str().unwrap());
        store::save(&test_creds()).unwrap();

        let mut server = mockito::Server::new_async().await;
        let _s = server
            .mock("POST", "/v1/advisor/query")
            .with_body(START_BODY)
            .with_header("content-type", "application/json")
            .create_async()
            .await;
        let _nm = server
            .mock("GET", POLL_PATH)
            .with_status(304)
            .expect(1)
            .create_async()
            .await;
        let _done = server
            .mock("GET", POLL_PATH)
            .with_body(succeeded_body(""))
            .with_header("content-type", "application/json")
            .create_async()
            .await;
        run(
            &args(&server.url(), None),
            None,
            Duration::from_secs(60),
            Duration::ZERO,
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn test_poll_times_out_at_deadline() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::remove("ACTUAL_CONFIG");
        let tmp = tempdir().unwrap();
        let _g2 = EnvGuard::set("ACTUAL_CONFIG_DIR", tmp.path().to_str().unwrap());
        store::save(&test_creds()).unwrap();

        let mut server = mockito::Server::new_async().await;
        let _s = server
            .mock("POST", "/v1/advisor/query")
            .with_body(START_BODY)
            .with_header("content-type", "application/json")
            .create_async()
            .await;
        // Always running → the loop keeps polling until the wall-clock deadline.
        let _p = server
            .mock("GET", POLL_PATH)
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_header("retry-after", "0")
            .with_body(r#"{"query_id":"q1","status":"running","result":null,"error":null}"#)
            .create_async()
            .await;
        // Tiny deadline + zero interval → polls a few times, then gives up.
        let err = run(
            &args(&server.url(), None),
            None,
            Duration::from_millis(10),
            Duration::ZERO,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, ActualError::ApiError(ref m) if m.contains("did not reach")));
    }

    #[tokio::test]
    async fn test_run_sends_versioned_job_envelope() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::remove("ACTUAL_CONFIG");
        let tmp = tempdir().unwrap();
        let _g2 = EnvGuard::set("ACTUAL_CONFIG_DIR", tmp.path().to_str().unwrap());
        store::save(&test_creds()).unwrap();

        let mut server = mockito::Server::new_async().await;
        // The server validates the typed/versioned envelope: type + version
        // literals and the query nested under `data`.
        let s = server
            .mock("POST", "/v1/advisor/query")
            .match_body(mockito::Matcher::PartialJsonString(
                r#"{"type":"advisor_query","version":1,"data":{"query":"why app router?"}}"#
                    .to_string(),
            ))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(START_BODY)
            .create_async()
            .await;
        let _p = server
            .mock("GET", POLL_PATH)
            .with_body(succeeded_body(""))
            .with_header("content-type", "application/json")
            .create_async()
            .await;

        run(
            &args(&server.url(), None),
            None,
            Duration::from_secs(60),
            Duration::ZERO,
        )
        .await
        .unwrap();
        s.assert_async().await;
    }

    #[tokio::test]
    async fn test_run_retries_on_transient_500() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::remove("ACTUAL_CONFIG");
        let tmp = tempdir().unwrap();
        let _g2 = EnvGuard::set("ACTUAL_CONFIG_DIR", tmp.path().to_str().unwrap());
        store::save(&test_creds()).unwrap();

        let mut server = mockito::Server::new_async().await;
        let _s = server
            .mock("POST", "/v1/advisor/query")
            .with_body(START_BODY)
            .with_header("content-type", "application/json")
            .create_async()
            .await;
        // First poll: transient infra 500 → retried, not fatal. Second: succeeded.
        let infra = server
            .mock("GET", POLL_PATH)
            .with_status(500)
            .with_body(r#"{"error":"row load failed"}"#)
            .expect(1)
            .create_async()
            .await;
        let _done = server
            .mock("GET", POLL_PATH)
            .with_body(succeeded_body(ONE_ADR))
            .with_header("content-type", "application/json")
            .create_async()
            .await;

        run(
            &args(&server.url(), None),
            None,
            Duration::from_secs(60),
            Duration::ZERO,
        )
        .await
        .unwrap();
        infra.assert_async().await;
    }

    #[test]
    fn test_next_delay_clamps_retry_after() {
        // A large (or misbehaving) server Retry-After is clamped to the ceiling.
        assert_eq!(
            next_delay(Some(600), Duration::from_secs(2)),
            MAX_RETRY_AFTER
        );
        assert_eq!(
            next_delay(Some(15), Duration::from_secs(2)),
            Duration::from_secs(15)
        );
        // Values under the ceiling pass through; None falls back to the default.
        assert_eq!(
            next_delay(Some(3), Duration::from_secs(2)),
            Duration::from_secs(3)
        );
        assert_eq!(
            next_delay(None, Duration::from_secs(2)),
            Duration::from_secs(2)
        );
    }

    // --- transparent refresh-on-expiry ---

    #[tokio::test]
    async fn test_ensure_fresh_no_refresh_token_returns_unchanged() {
        let mut c = test_creds();
        c.refresh_token = String::new();
        let out = ensure_fresh(c.clone()).await.unwrap();
        assert_eq!(out.access_token, c.access_token);
    }

    #[tokio::test]
    async fn test_ensure_fresh_not_expired_returns_unchanged() {
        let mut c = test_creds();
        c.expires_at = Some(Utc::now() + ChronoDuration::hours(1));
        let out = ensure_fresh(c).await.unwrap();
        assert_eq!(out.access_token, "tok");
    }

    #[tokio::test]
    async fn test_ensure_fresh_expired_refreshes_and_persists() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::remove("ACTUAL_CONFIG");
        let tmp = tempdir().unwrap();
        let _g2 = EnvGuard::set("ACTUAL_CONFIG_DIR", tmp.path().to_str().unwrap());

        let mut server = mockito::Server::new_async().await;
        let _m = server
            .mock("POST", "/api/oauth/token")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"access_token":"new-at","token_type":"Bearer","expires_in":3600,"refresh_token":"new-rt"}"#,
            )
            .create_async()
            .await;

        let mut c = test_creds();
        c.expires_at = Some(Utc::now() - ChronoDuration::seconds(1)); // expired
        c.auth_url = Some(server.url());

        let out = ensure_fresh(c).await.unwrap();
        assert_eq!(out.access_token, "new-at");
        // Rotated creds were re-persisted.
        assert_eq!(store::load().unwrap().unwrap().access_token, "new-at");
    }

    #[tokio::test]
    async fn test_ensure_fresh_refresh_failure_is_not_logged_in() {
        // Expired + unreachable auth server → refresh errors → NotLoggedIn.
        let mut c = test_creds();
        c.expires_at = Some(Utc::now() - ChronoDuration::seconds(1));
        c.auth_url = Some("http://127.0.0.1:1".to_string());
        let err = ensure_fresh(c).await.unwrap_err();
        assert!(matches!(err, ActualError::NotLoggedIn));
    }

    // --- explicit repo scoping ---

    #[tokio::test]
    async fn test_run_with_explicit_repo_scopes_request() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::remove("ACTUAL_CONFIG");
        let tmp = tempdir().unwrap();
        let _g2 = EnvGuard::set("ACTUAL_CONFIG_DIR", tmp.path().to_str().unwrap());
        store::save(&test_creds()).unwrap();

        let mut server = mockito::Server::new_async().await;
        let s = server
            .mock("POST", "/v1/advisor/query")
            .match_body(mockito::Matcher::PartialJsonString(
                r#"{"repo_unique_id":"33333333-3333-3333-3333-333333333333"}"#.to_string(),
            ))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(START_BODY)
            .create_async()
            .await;
        let _p = server
            .mock("GET", POLL_PATH)
            .with_body(succeeded_body(""))
            .with_header("content-type", "application/json")
            .create_async()
            .await;

        let mut a = args(&server.url(), None);
        a.repo = Some("33333333-3333-3333-3333-333333333333".to_string());
        run(&a, None, Duration::from_secs(60), Duration::ZERO)
            .await
            .unwrap();
        s.assert_async().await;
    }

    // --- cross-org 403 handling ---

    #[tokio::test]
    async fn test_run_org_mismatch_403_surfaces_actionable_error() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::remove("ACTUAL_CONFIG");
        let tmp = tempdir().unwrap();
        let _g2 = EnvGuard::set("ACTUAL_CONFIG_DIR", tmp.path().to_str().unwrap());
        store::save(&test_creds()).unwrap();

        let mut server = mockito::Server::new_async().await;
        // api-service rejects the cross-org token with a fail-closed 403.
        let _s = server
            .mock("POST", "/v1/advisor/query")
            .with_status(403)
            .with_header("content-type", "application/json")
            .with_body(r#"{"error":{"code":"FORBIDDEN","message":"cross-org","details":null}}"#)
            .create_async()
            .await;

        // Explicit --org that differs from the session org (test_creds is 1111…).
        let target = "99999999-9999-9999-9999-999999999999";
        let err = run(
            &args(&server.url(), Some(target)),
            None,
            Duration::from_secs(60),
            Duration::ZERO,
        )
        .await
        .unwrap_err();

        match err {
            ActualError::OrgMismatch { message, hint } => {
                assert!(message.contains("403"), "expected 403 in: {message}");
                assert!(
                    message.contains(target),
                    "expected target org in: {message}"
                );
                assert!(
                    message.contains("11111111-1111-1111-1111-111111111111"),
                    "expected session org in: {message}"
                );
                assert!(
                    hint.contains("actual login --org"),
                    "expected actionable remediation in hint: {hint}"
                );
            }
            other => panic!("expected OrgMismatch, got {other:?}"),
        }
    }

    #[test]
    fn test_org_mismatch_message_explicit_org_names_both_and_remediation() {
        let (message, hint) = org_mismatch_message("org-A", "org-B", true);
        assert!(
            message.contains("org-A"),
            "expected session org in: {message}"
        );
        assert!(
            message.contains("org-B"),
            "expected target org in: {message}"
        );
        assert!(message.contains("403"), "expected 403 in: {message}");
        // The remediation now rides on the hint (the "Fix:" line), not Display.
        assert!(
            hint.contains("actual login --org org-B"),
            "expected targeted remediation in hint: {hint}"
        );
        assert!(
            !message.contains("actual login"),
            "remediation should not be in the message: {message}"
        );
    }

    #[test]
    fn test_org_mismatch_message_no_explicit_org_steers_to_relogin() {
        // No explicit --org → target == session; the generic re-login branch.
        let (message, hint) = org_mismatch_message("org-A", "org-A", false);
        assert!(
            message.contains("org-A"),
            "expected session org in: {message}"
        );
        assert!(message.contains("403"), "expected 403 in: {message}");
        // Remediation rides on the hint, not Display.
        assert!(
            hint.contains("actual login"),
            "expected re-login remediation in hint: {hint}"
        );
        assert!(
            !message.contains("actual login"),
            "remediation should not be in the message: {message}"
        );
    }

    // --- --repo name resolution ---

    fn repo(owner: &str, name: &str, id: &str) -> ConnectedRepository {
        ConnectedRepository {
            repo_unique_id: id.to_string(),
            name: name.to_string(),
            external_owner: owner.to_string(),
            url: format!("https://github.com/{owner}/{name}"),
        }
    }

    #[test]
    fn test_qualified_name_is_owner_slash_name() {
        assert_eq!(
            qualified_name(&repo("actual-software", "actual-cli", "id")),
            "actual-software/actual-cli"
        );
    }

    #[test]
    fn test_resolve_repo_name_by_bare_name() {
        let repos = vec![
            repo("actual-software", "actual-cli", "id-cli"),
            repo("actual-software", "web-app", "id-web"),
        ];
        assert_eq!(resolve_repo_name("actual-cli", &repos).unwrap(), "id-cli");
    }

    #[test]
    fn test_resolve_repo_name_by_owner_slash_name() {
        // Two repos share the short name; the owner-qualified form disambiguates.
        let repos = vec![
            repo("actual-software", "cli", "id-a"),
            repo("other-org", "cli", "id-b"),
        ];
        assert_eq!(resolve_repo_name("other-org/cli", &repos).unwrap(), "id-b");
    }

    #[test]
    fn test_resolve_repo_name_is_case_insensitive() {
        // GitHub owner/repo names are case-insensitive in practice; a
        // differently-cased --repo value still resolves, in both the bare-name
        // and owner-qualified forms.
        let repos = vec![repo("actual-software", "actual-cli", "id-cli")];
        assert_eq!(resolve_repo_name("ACTUAL-CLI", &repos).unwrap(), "id-cli");
        assert_eq!(
            resolve_repo_name("Actual-Software/Actual-CLI", &repos).unwrap(),
            "id-cli"
        );
    }

    #[test]
    fn test_resolve_repo_name_not_found_lists_connected_repos() {
        let repos = vec![
            repo("actual-software", "actual-cli", "id-cli"),
            repo("actual-software", "web-app", "id-web"),
        ];
        let err = resolve_repo_name("nope", &repos).unwrap_err();
        assert!(matches!(err, ActualError::RepoNotFound(_)), "got: {err:?}");
        // RepoNotFound Displays as its message ({0}); assert on that.
        let msg = err.to_string();
        assert!(msg.contains("nope"), "got: {msg}");
        assert!(msg.contains("actual-software/actual-cli"), "got: {msg}");
        assert!(msg.contains("actual-software/web-app"), "got: {msg}");
    }

    #[test]
    fn test_resolve_repo_name_ambiguous_bare_name() {
        // A bare name shared across owners is ambiguous and asks for owner/name.
        let repos = vec![
            repo("actual-software", "cli", "id-a"),
            repo("other-org", "cli", "id-b"),
        ];
        let err = resolve_repo_name("cli", &repos).unwrap_err();
        assert!(matches!(err, ActualError::RepoNotFound(_)), "got: {err:?}");
        let msg = err.to_string();
        assert!(msg.contains("multiple"), "got: {msg}");
        assert!(msg.contains("actual-software/cli"), "got: {msg}");
        assert!(msg.contains("other-org/cli"), "got: {msg}");
    }

    #[test]
    fn test_not_found_message_when_org_has_no_connected_repos() {
        let msg = not_found_message("anything", &[]);
        assert!(msg.contains("anything"), "got: {msg}");
        assert!(
            msg.contains("no connected repositories"),
            "expected empty-org phrasing in: {msg}"
        );
    }

    #[tokio::test]
    async fn test_resolve_repo_uuid_passes_through_without_lookup() {
        // A UUID short-circuits: the unreachable client is never called.
        let client = ActualApiClient::new("http://127.0.0.1:1")
            .unwrap()
            .with_bearer("t");
        let id = "33333333-3333-3333-3333-333333333333";
        let resolved = resolve_repo(id, &client, "org").await.unwrap();
        assert_eq!(resolved.repo_unique_id, id);
        // A bare UUID resolves without a name lookup.
        assert!(resolved.qualified_name.is_none());
    }

    #[tokio::test]
    async fn test_resolve_repo_by_name_calls_connected_repos_api() {
        let mut server = mockito::Server::new_async().await;
        let m = server
            .mock("GET", "/v1/connected-repos")
            .match_query(mockito::Matcher::Any)
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"repositories":[{"repo_unique_id":"33333333-3333-3333-3333-333333333333","name":"actual-cli","external_owner":"actual-software","url":"https://github.com/actual-software/actual-cli"}]}"#,
            )
            .create_async()
            .await;
        let client = ActualApiClient::new(&server.url())
            .unwrap()
            .with_bearer("t");
        let resolved = resolve_repo("actual-cli", &client, "org").await.unwrap();
        assert_eq!(
            resolved.repo_unique_id,
            "33333333-3333-3333-3333-333333333333"
        );
        // The name lookup also yields the repo's owner/name for display + pinning.
        assert_eq!(
            resolved.qualified_name.as_deref(),
            Some("actual-software/actual-cli")
        );
        m.assert_async().await;
    }

    #[tokio::test]
    async fn test_run_with_repo_name_resolves_and_scopes_request() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::remove("ACTUAL_CONFIG");
        let tmp = tempdir().unwrap();
        let _g2 = EnvGuard::set("ACTUAL_CONFIG_DIR", tmp.path().to_str().unwrap());
        store::save(&test_creds()).unwrap();

        let mut server = mockito::Server::new_async().await;
        // Name → id lookup happens first.
        let repos = server
            .mock("GET", "/v1/connected-repos")
            .match_query(mockito::Matcher::Any)
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"repositories":[{"repo_unique_id":"33333333-3333-3333-3333-333333333333","name":"actual-cli","external_owner":"actual-software","url":"https://github.com/actual-software/actual-cli"}]}"#,
            )
            .create_async()
            .await;
        // The advisor request then carries the resolved repo id.
        let start = server
            .mock("POST", "/v1/advisor/query")
            .match_body(mockito::Matcher::PartialJsonString(
                r#"{"repo_unique_id":"33333333-3333-3333-3333-333333333333"}"#.to_string(),
            ))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(START_BODY)
            .create_async()
            .await;
        let _poll = server
            .mock("GET", POLL_PATH)
            .with_body(succeeded_body(""))
            .with_header("content-type", "application/json")
            .create_async()
            .await;

        let mut a = args(&server.url(), None);
        a.repo = Some("actual-cli".to_string());
        run(&a, None, Duration::from_secs(60), Duration::ZERO)
            .await
            .unwrap();
        repos.assert_async().await;
        start.assert_async().await;
    }

    #[tokio::test]
    async fn test_run_with_unknown_repo_name_errors_before_query() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::remove("ACTUAL_CONFIG");
        let tmp = tempdir().unwrap();
        let _g2 = EnvGuard::set("ACTUAL_CONFIG_DIR", tmp.path().to_str().unwrap());
        store::save(&test_creds()).unwrap();

        let mut server = mockito::Server::new_async().await;
        let _repos = server
            .mock("GET", "/v1/connected-repos")
            .match_query(mockito::Matcher::Any)
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"repositories":[]}"#)
            .create_async()
            .await;

        let mut a = args(&server.url(), None);
        a.repo = Some("ghost".to_string());
        let err = run(&a, None, Duration::from_secs(60), Duration::ZERO)
            .await
            .unwrap_err();
        assert!(
            matches!(err, ActualError::RepoNotFound(ref m) if m.contains("ghost")),
            "got: {err:?}"
        );
    }

    // --- git-remote auto-detection ---

    /// Run a git command in `cwd`, asserting it succeeds. Used to build the
    /// throwaway repos the detection tests read `origin` from.
    fn run_git(cwd: &Path, git_args: &[&str]) {
        let status = std::process::Command::new("git")
            .args(git_args)
            .current_dir(cwd)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .expect("git is available in the test environment");
        assert!(status.success(), "git {git_args:?} failed");
    }

    /// A throwaway git repo whose `origin` remote is `url`. No commit is needed —
    /// `git remote get-url origin` works right after `git remote add`.
    fn git_repo_with_remote(url: &str) -> tempfile::TempDir {
        let dir = tempdir().unwrap();
        run_git(dir.path(), &["init", "-q"]);
        run_git(dir.path(), &["remote", "add", "origin", url]);
        dir
    }

    #[test]
    fn test_parse_git_remote_url_forms() {
        let expect = |o: &str, n: &str| {
            Some(RepoRemote {
                owner: o.to_string(),
                name: n.to_string(),
            })
        };
        // SSH scp-like, with and without the .git suffix.
        assert_eq!(
            parse_git_remote_url("git@github.com:actual-software/actual-cli.git"),
            expect("actual-software", "actual-cli")
        );
        assert_eq!(
            parse_git_remote_url("git@github.com:actual-software/actual-cli"),
            expect("actual-software", "actual-cli")
        );
        // HTTPS, with .git / without / with a trailing slash.
        assert_eq!(
            parse_git_remote_url("https://github.com/actual-software/actual-cli.git"),
            expect("actual-software", "actual-cli")
        );
        assert_eq!(
            parse_git_remote_url("https://github.com/actual-software/actual-cli"),
            expect("actual-software", "actual-cli")
        );
        assert_eq!(
            parse_git_remote_url("https://github.com/actual-software/actual-cli/"),
            expect("actual-software", "actual-cli")
        );
        // ssh:// URL form carrying a port segment.
        assert_eq!(
            parse_git_remote_url("ssh://git@github.com:22/actual-software/actual-cli.git"),
            expect("actual-software", "actual-cli")
        );
        // Surrounding whitespace is trimmed.
        assert_eq!(
            parse_git_remote_url("  git@github.com:actual-software/actual-cli.git\n"),
            expect("actual-software", "actual-cli")
        );
    }

    #[test]
    fn test_parse_git_remote_url_rejects_unparseable() {
        assert!(parse_git_remote_url("").is_none());
        assert!(parse_git_remote_url("garbage").is_none());
        assert!(parse_git_remote_url("/").is_none());
    }

    #[test]
    fn test_repo_remote_debug_eq_and_slug() {
        let a = RepoRemote {
            owner: "o".to_string(),
            name: "n".to_string(),
        };
        // eq false paths: name differs, then owner differs.
        assert_ne!(
            a,
            RepoRemote {
                owner: "o".to_string(),
                name: "other".to_string()
            }
        );
        assert_ne!(
            a,
            RepoRemote {
                owner: "x".to_string(),
                name: "n".to_string()
            }
        );
        // eq true path.
        assert_eq!(
            a,
            RepoRemote {
                owner: "o".to_string(),
                name: "n".to_string()
            }
        );
        // Debug + slug.
        assert!(format!("{a:?}").contains('o'));
        assert_eq!(a.slug(), "o/n");
    }

    #[test]
    fn test_short_id_takes_first_eight_chars() {
        assert_eq!(short_id("33333333-3333-3333-3333-333333333333"), "33333333");
        // A value shorter than 8 chars is returned whole.
        assert_eq!(short_id("abc"), "abc");
    }

    #[test]
    fn test_match_remote_exact_owner_and_name() {
        let remote = RepoRemote {
            owner: "actual-software".to_string(),
            name: "actual-cli".to_string(),
        };
        let repos = vec![
            repo("actual-software", "actual-cli", "id-cli"),
            repo("other", "web", "id-web"),
        ];
        let m = match_remote_to_repos(&remote, &repos);
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].repo_unique_id, "id-cli");
    }

    #[test]
    fn test_match_remote_is_case_insensitive() {
        let remote = RepoRemote {
            owner: "Actual-Software".to_string(),
            name: "Actual-CLI".to_string(),
        };
        let repos = vec![repo("actual-software", "actual-cli", "id-cli")];
        let m = match_remote_to_repos(&remote, &repos);
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].repo_unique_id, "id-cli");
    }

    #[test]
    fn test_match_remote_does_not_guess_from_name_only() {
        // A different owner is not authoritative evidence that this is a fork
        // of the connected repo. A coincidental name must not silently scope
        // an advisor query to unrelated repository data.
        let remote = RepoRemote {
            owner: "unrelated-owner".to_string(),
            name: "actual-cli".to_string(),
        };
        let repos = vec![
            repo("actual-software", "actual-cli", "id-cli"),
            repo("other", "web", "id-web"),
        ];
        assert!(match_remote_to_repos(&remote, &repos).is_empty());
    }

    #[test]
    fn test_match_remote_empty_when_nothing_matches() {
        let remote = RepoRemote {
            owner: "who".to_string(),
            name: "unknown".to_string(),
        };
        let repos = vec![repo("actual-software", "actual-cli", "id-cli")];
        assert!(match_remote_to_repos(&remote, &repos).is_empty());
    }

    #[test]
    fn test_match_remote_ignores_shared_name_without_owner_match() {
        // Neither same-name repository has the remote's owner, so neither is
        // safe to select.
        let remote = RepoRemote {
            owner: "my-fork".to_string(),
            name: "cli".to_string(),
        };
        let repos = vec![
            repo("actual-software", "cli", "id-a"),
            repo("other-org", "cli", "id-b"),
        ];
        assert!(match_remote_to_repos(&remote, &repos).is_empty());
    }

    #[tokio::test]
    async fn test_origin_remote_url_reads_configured_remote() {
        let dir = git_repo_with_remote("git@github.com:actual-software/actual-cli.git");
        assert_eq!(
            crate::git::origin_remote_url(dir.path(), GIT_REMOTE_TIMEOUT)
                .await
                .as_deref(),
            Some("git@github.com:actual-software/actual-cli.git")
        );
    }

    #[tokio::test]
    async fn test_origin_remote_url_none_outside_repo() {
        // A plain temp dir is not a git repo → git exits non-zero → None.
        let dir = tempdir().unwrap();
        assert!(
            crate::git::origin_remote_url(dir.path(), GIT_REMOTE_TIMEOUT)
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn test_auto_detect_repo_scopes_on_single_match() {
        let dir = git_repo_with_remote("git@github.com:actual-software/actual-cli.git");
        let mut server = mockito::Server::new_async().await;
        let m = server
            .mock("GET", "/v1/connected-repos")
            .match_query(mockito::Matcher::Any)
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"repositories":[{"repo_unique_id":"33333333-3333-3333-3333-333333333333","name":"actual-cli","external_owner":"actual-software","url":"https://github.com/actual-software/actual-cli"}]}"#,
            )
            .create_async()
            .await;
        let client = ActualApiClient::new(&server.url())
            .unwrap()
            .with_bearer("t");
        let id = auto_detect_repo(dir.path(), &client, "org").await;
        assert_eq!(id.as_deref(), Some("33333333-3333-3333-3333-333333333333"));
        m.assert_async().await;
    }

    #[tokio::test]
    async fn test_auto_detect_repo_org_fallback_when_no_match() {
        let dir = git_repo_with_remote("git@github.com:someone/unconnected.git");
        let mut server = mockito::Server::new_async().await;
        let _m = server
            .mock("GET", "/v1/connected-repos")
            .match_query(mockito::Matcher::Any)
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"repositories":[{"repo_unique_id":"33333333-3333-3333-3333-333333333333","name":"actual-cli","external_owner":"actual-software","url":"u"}]}"#,
            )
            .create_async()
            .await;
        let client = ActualApiClient::new(&server.url())
            .unwrap()
            .with_bearer("t");
        assert!(auto_detect_repo(dir.path(), &client, "org").await.is_none());
    }

    #[tokio::test]
    async fn test_auto_detect_repo_org_fallback_when_ambiguous() {
        let dir = git_repo_with_remote("git@github.com:my-fork/cli.git");
        let mut server = mockito::Server::new_async().await;
        let _m = server
            .mock("GET", "/v1/connected-repos")
            .match_query(mockito::Matcher::Any)
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"repositories":[{"repo_unique_id":"a","name":"cli","external_owner":"my-fork","url":"u1"},{"repo_unique_id":"b","name":"cli","external_owner":"my-fork","url":"u2"}]}"#,
            )
            .create_async()
            .await;
        let client = ActualApiClient::new(&server.url())
            .unwrap()
            .with_bearer("t");
        assert!(auto_detect_repo(dir.path(), &client, "org").await.is_none());
    }

    #[tokio::test]
    async fn test_auto_detect_repo_org_fallback_on_api_error() {
        let dir = git_repo_with_remote("git@github.com:actual-software/actual-cli.git");
        let mut server = mockito::Server::new_async().await;
        let _m = server
            .mock("GET", "/v1/connected-repos")
            .match_query(mockito::Matcher::Any)
            .with_status(500)
            .with_header("content-type", "application/json")
            .with_body(r#"{"error":"boom","message":"kaboom"}"#)
            .create_async()
            .await;
        let client = ActualApiClient::new(&server.url())
            .unwrap()
            .with_bearer("t");
        assert!(auto_detect_repo(dir.path(), &client, "org").await.is_none());
    }

    #[tokio::test]
    async fn test_auto_detect_repo_no_remote_short_circuits() {
        // No git repo → no remote → the connected-repos API is never called, so an
        // unreachable client still yields None (proves the short-circuit).
        let dir = tempdir().unwrap();
        let client = ActualApiClient::new("http://127.0.0.1:1")
            .unwrap()
            .with_bearer("t");
        assert!(auto_detect_repo(dir.path(), &client, "org").await.is_none());
    }

    #[tokio::test]
    async fn test_auto_detect_repo_unparseable_remote_short_circuits() {
        // An origin that yields no owner/name → the API is never called.
        let dir = git_repo_with_remote("garbage");
        let client = ActualApiClient::new("http://127.0.0.1:1")
            .unwrap()
            .with_bearer("t");
        assert!(auto_detect_repo(dir.path(), &client, "org").await.is_none());
    }

    #[tokio::test]
    async fn test_run_auto_detects_repo_and_scopes_request() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::remove("ACTUAL_CONFIG");
        let tmp = tempdir().unwrap();
        let _g2 = EnvGuard::set("ACTUAL_CONFIG_DIR", tmp.path().to_str().unwrap());
        store::save(&test_creds()).unwrap();

        let repo_dir = git_repo_with_remote("git@github.com:actual-software/actual-cli.git");

        let mut server = mockito::Server::new_async().await;
        let repos = server
            .mock("GET", "/v1/connected-repos")
            .match_query(mockito::Matcher::Any)
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"repositories":[{"repo_unique_id":"33333333-3333-3333-3333-333333333333","name":"actual-cli","external_owner":"actual-software","url":"https://github.com/actual-software/actual-cli"}]}"#,
            )
            .create_async()
            .await;
        // The advisor request must carry the auto-detected repo id.
        let start = server
            .mock("POST", "/v1/advisor/query")
            .match_body(mockito::Matcher::PartialJsonString(
                r#"{"repo_unique_id":"33333333-3333-3333-3333-333333333333"}"#.to_string(),
            ))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(START_BODY)
            .create_async()
            .await;
        let _poll = server
            .mock("GET", POLL_PATH)
            .with_body(succeeded_body(""))
            .with_header("content-type", "application/json")
            .create_async()
            .await;

        // --repo omitted → auto-detected from the git repo's origin remote.
        run(
            &args(&server.url(), None),
            Some(repo_dir.path()),
            Duration::from_secs(60),
            Duration::ZERO,
        )
        .await
        .unwrap();
        repos.assert_async().await;
        start.assert_async().await;
    }

    // ---- sticky repo scope ----

    #[test]
    fn test_format_repo_scope_with_and_without_name() {
        assert_eq!(
            format_repo_scope(Some("actual-software/actual-cli"), "abcdef1234567890"),
            "actual-software/actual-cli (abcdef12)"
        );
        assert_eq!(format_repo_scope(None, "abcdef1234567890"), "repo abcdef12");
    }

    #[test]
    fn test_apply_remembered_repo_and_org() {
        let repo = apply_remembered(&StickyScope::repo("id-123456789", Some("o/r".to_string())));
        assert_eq!(repo.as_deref(), Some("id-123456789"));
        assert!(apply_remembered(&StickyScope::org_level()).is_none());
    }

    #[tokio::test]
    async fn test_sticky_key_none_dir_is_none() {
        assert!(sticky_key(None).await.is_none());
    }

    #[tokio::test]
    async fn test_sticky_key_hashes_origin_remote_deterministically() {
        let repo_dir = git_repo_with_remote("git@github.com:actual-software/actual-cli.git");
        let key = sticky_key(Some(repo_dir.path())).await.unwrap();
        assert_eq!(key.len(), 64, "sha256 hex is 64 chars");
        assert!(key.chars().all(|c| c.is_ascii_hexdigit()));
        // Same remote → same key.
        assert_eq!(key, sticky_key(Some(repo_dir.path())).await.unwrap());
    }

    #[tokio::test]
    async fn test_sticky_key_no_remote_falls_back_to_path() {
        // A dir with no git repo has no origin remote, so the key hashes the path.
        let dir = tempdir().unwrap();
        let key = sticky_key(Some(dir.path())).await.unwrap();
        assert_eq!(key.len(), 64);
    }

    fn scope_client() -> ActualApiClient {
        // An unreachable client for scope paths that never touch the network.
        ActualApiClient::new("http://127.0.0.1:1")
            .unwrap()
            .with_bearer("t")
    }

    #[tokio::test]
    async fn test_determine_scope_repo_none_pins_org_level() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::remove("ACTUAL_CONFIG");
        let tmp = tempdir().unwrap();
        let _g2 = EnvGuard::set("ACTUAL_CONFIG_DIR", tmp.path().to_str().unwrap());

        let mut a = args("http://127.0.0.1:1", None);
        a.query = None;
        a.repo = Some("none".to_string());
        let scope = determine_scope(&a, Some("key1"), None, &scope_client(), "org", "org", false)
            .await
            .unwrap();
        assert!(scope.is_none());

        let cfg = crate::config::paths::load().unwrap();
        assert_eq!(
            sticky::get_scope(&cfg, "key1"),
            Some(StickyScope::org_level())
        );
    }

    #[tokio::test]
    async fn test_determine_scope_repo_auto_clears_pin_and_autodetects() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::remove("ACTUAL_CONFIG");
        let tmp = tempdir().unwrap();
        let _g2 = EnvGuard::set("ACTUAL_CONFIG_DIR", tmp.path().to_str().unwrap());

        let mut cfg = crate::config::paths::load().unwrap();
        sticky::set_scope(&mut cfg, "key1", StickyScope::repo("old-id", None));
        crate::config::paths::save(&cfg).unwrap();

        let mut a = args("http://127.0.0.1:1", None);
        a.query = None;
        a.repo = Some("auto".to_string());
        // repo_dir None → autodetect yields None; the pin is forgotten.
        let scope = determine_scope(&a, Some("key1"), None, &scope_client(), "org", "org", false)
            .await
            .unwrap();
        assert!(scope.is_none());

        let cfg = crate::config::paths::load().unwrap();
        assert!(sticky::get_scope(&cfg, "key1").is_none());
    }

    #[tokio::test]
    async fn test_determine_scope_repo_auto_without_repo_key() {
        // No repo key (no working dir): nothing to forget, still auto-detects
        // (with repo_dir None → org-level). Covers the `auto` arm's no-key path.
        let mut a = args("http://127.0.0.1:1", None);
        a.query = None;
        a.repo = Some("auto".to_string());
        let scope = determine_scope(&a, None, None, &scope_client(), "org", "org", false)
            .await
            .unwrap();
        assert!(scope.is_none());
    }

    #[tokio::test]
    async fn test_determine_scope_explicit_repo_persists_pin() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::remove("ACTUAL_CONFIG");
        let tmp = tempdir().unwrap();
        let _g2 = EnvGuard::set("ACTUAL_CONFIG_DIR", tmp.path().to_str().unwrap());

        let mut server = mockito::Server::new_async().await;
        let m = server
            .mock("GET", "/v1/connected-repos")
            .match_query(mockito::Matcher::Any)
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"repositories":[{"repo_unique_id":"33333333-3333-3333-3333-333333333333","name":"actual-cli","external_owner":"actual-software","url":"https://github.com/actual-software/actual-cli"}]}"#,
            )
            .create_async()
            .await;
        let client = ActualApiClient::new(&server.url())
            .unwrap()
            .with_bearer("t");

        let mut a = args(&server.url(), None);
        a.query = None;
        a.repo = Some("actual-cli".to_string());
        let scope = determine_scope(&a, Some("key1"), None, &client, "org", "org", false)
            .await
            .unwrap();
        assert_eq!(
            scope.as_deref(),
            Some("33333333-3333-3333-3333-333333333333")
        );
        m.assert_async().await;

        let cfg = crate::config::paths::load().unwrap();
        let pinned = sticky::get_scope(&cfg, "key1").unwrap();
        assert_eq!(
            pinned.repo_unique_id.as_deref(),
            Some("33333333-3333-3333-3333-333333333333")
        );
        assert_eq!(
            pinned.qualified_name.as_deref(),
            Some("actual-software/actual-cli")
        );
    }

    #[tokio::test]
    async fn test_determine_scope_explicit_org_overrides_pin() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::remove("ACTUAL_CONFIG");
        let tmp = tempdir().unwrap();
        let _g2 = EnvGuard::set("ACTUAL_CONFIG_DIR", tmp.path().to_str().unwrap());

        // A repo pin exists, but an explicit --org runs org-level for this call.
        let mut cfg = crate::config::paths::load().unwrap();
        sticky::set_scope(&mut cfg, "key1", StickyScope::repo("pinned-id", None));
        crate::config::paths::save(&cfg).unwrap();

        let a = args("http://127.0.0.1:1", Some("some-org"));
        let scope = determine_scope(
            &a,
            Some("key1"),
            None,
            &scope_client(),
            "some-org",
            "sess",
            true,
        )
        .await
        .unwrap();
        assert!(
            scope.is_none(),
            "explicit --org wins over the remembered pin"
        );
        // The pin is untouched by a one-shot --org.
        let cfg = crate::config::paths::load().unwrap();
        assert!(sticky::get_scope(&cfg, "key1").is_some());
    }

    #[tokio::test]
    async fn test_determine_scope_uses_remembered_repo_pin() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::remove("ACTUAL_CONFIG");
        let tmp = tempdir().unwrap();
        let _g2 = EnvGuard::set("ACTUAL_CONFIG_DIR", tmp.path().to_str().unwrap());

        let mut cfg = crate::config::paths::load().unwrap();
        sticky::set_scope(
            &mut cfg,
            "key1",
            StickyScope::repo("pinned-id", Some("o/r".to_string())),
        );
        crate::config::paths::save(&cfg).unwrap();

        // No --repo, no --org: the remembered pin is reused (client never called).
        let a = args("http://127.0.0.1:1", None);
        let scope = determine_scope(&a, Some("key1"), None, &scope_client(), "org", "org", false)
            .await
            .unwrap();
        assert_eq!(scope.as_deref(), Some("pinned-id"));
    }

    #[tokio::test]
    async fn test_determine_scope_uses_remembered_org_pin() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::remove("ACTUAL_CONFIG");
        let tmp = tempdir().unwrap();
        let _g2 = EnvGuard::set("ACTUAL_CONFIG_DIR", tmp.path().to_str().unwrap());

        let mut cfg = crate::config::paths::load().unwrap();
        sticky::set_scope(&mut cfg, "key1", StickyScope::org_level());
        crate::config::paths::save(&cfg).unwrap();

        let a = args("http://127.0.0.1:1", None);
        let scope = determine_scope(&a, Some("key1"), None, &scope_client(), "org", "org", false)
            .await
            .unwrap();
        assert!(scope.is_none());
    }

    #[test]
    fn test_show_scope_no_key_reports_not_pinned() {
        // No repo key (e.g. no working directory) → no pin to read.
        show_scope(None).unwrap();
    }

    #[test]
    fn test_show_scope_not_pinned() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::remove("ACTUAL_CONFIG");
        let tmp = tempdir().unwrap();
        let _g2 = EnvGuard::set("ACTUAL_CONFIG_DIR", tmp.path().to_str().unwrap());
        show_scope(Some("unpinned-key")).unwrap();
    }

    #[test]
    fn test_show_scope_pinned_repo_and_org() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::remove("ACTUAL_CONFIG");
        let tmp = tempdir().unwrap();
        let _g2 = EnvGuard::set("ACTUAL_CONFIG_DIR", tmp.path().to_str().unwrap());

        let mut cfg = crate::config::paths::load().unwrap();
        sticky::set_scope(
            &mut cfg,
            "repo-key",
            StickyScope::repo("id123456789", Some("o/r".to_string())),
        );
        sticky::set_scope(&mut cfg, "org-key", StickyScope::org_level());
        crate::config::paths::save(&cfg).unwrap();

        show_scope(Some("repo-key")).unwrap();
        show_scope(Some("org-key")).unwrap();
    }

    #[tokio::test]
    async fn test_run_reuses_remembered_scope_without_repo_flag() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::remove("ACTUAL_CONFIG");
        let tmp = tempdir().unwrap();
        let _g2 = EnvGuard::set("ACTUAL_CONFIG_DIR", tmp.path().to_str().unwrap());
        store::save(&test_creds()).unwrap();

        let repo_dir = git_repo_with_remote("git@github.com:actual-software/actual-cli.git");
        // Pin the scope under this repo's key, then bare-query from the same dir.
        let key = sticky_key(Some(repo_dir.path())).await.unwrap();
        let mut cfg = crate::config::paths::load().unwrap();
        sticky::set_scope(
            &mut cfg,
            &key,
            StickyScope::repo(
                "77777777-7777-7777-7777-777777777777",
                Some("actual-software/actual-cli".to_string()),
            ),
        );
        crate::config::paths::save(&cfg).unwrap();

        let mut server = mockito::Server::new_async().await;
        let start = server
            .mock("POST", "/v1/advisor/query")
            .match_body(mockito::Matcher::PartialJsonString(
                r#"{"repo_unique_id":"77777777-7777-7777-7777-777777777777"}"#.to_string(),
            ))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(START_BODY)
            .create_async()
            .await;
        let _poll = server
            .mock("GET", POLL_PATH)
            .with_body(succeeded_body(""))
            .with_header("content-type", "application/json")
            .create_async()
            .await;

        run(
            &args(&server.url(), None),
            Some(repo_dir.path()),
            Duration::from_secs(60),
            Duration::ZERO,
        )
        .await
        .unwrap();
        start.assert_async().await;
    }

    #[tokio::test]
    async fn test_run_show_scope_exits_without_query() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::remove("ACTUAL_CONFIG");
        let tmp = tempdir().unwrap();
        let _g2 = EnvGuard::set("ACTUAL_CONFIG_DIR", tmp.path().to_str().unwrap());
        store::save(&test_creds()).unwrap();

        let mut a = args("http://127.0.0.1:1", None);
        a.query = None;
        a.show_scope = true;
        // No server: --show-scope reads local config and returns before any query.
        run(&a, None, Duration::from_secs(60), Duration::ZERO)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn test_run_repo_management_only_exits_without_query() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::remove("ACTUAL_CONFIG");
        let tmp = tempdir().unwrap();
        let _g2 = EnvGuard::set("ACTUAL_CONFIG_DIR", tmp.path().to_str().unwrap());
        store::save(&test_creds()).unwrap();

        let mut a = args("http://127.0.0.1:1", None);
        a.query = None;
        a.repo = Some("none".to_string());
        // A --repo change with no question applies the scope and returns; no query.
        run(&a, None, Duration::from_secs(60), Duration::ZERO)
            .await
            .unwrap();
    }
}
