//! `actual advisor <query>` — ask the Advisor org-scoped architecture questions.
//!
//! Starts an async advisor job, polls to completion (honoring the server's
//! `Retry-After` and retrying transient network/5xx errors up to a ~5-minute
//! cap), and renders the answer. Uses the platform token from `actual login`
//! as the bearer.

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
use crate::error::ActualError;

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

pub fn exec(args: &AdvisorArgs) -> Result<(), ActualError> {
    build_runtime()?.block_on(run(args, HARD_TIMEOUT, DEFAULT_POLL_INTERVAL))
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

/// Resolve a `--repo` value to a `repo_unique_id`. A value that parses as a UUID
/// is already an id and is returned as-is (no lookup, backward compatible); any
/// other value is treated as a repository name and resolved against the org's
/// connected repositories via `GET /v1/connected-repos`.
async fn resolve_repo(
    value: &str,
    client: &ActualApiClient,
    org_id: &str,
) -> Result<String, ActualError> {
    if Uuid::parse_str(value).is_ok() {
        return Ok(value.to_string());
    }
    let repos = client.list_connected_repos(org_id).await?;
    resolve_repo_name(value, &repos)
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
            .filter(|r| r.external_owner == owner && r.name == name)
            .collect(),
        None => repos.iter().filter(|r| r.name == value).collect(),
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

async fn run(
    args: &AdvisorArgs,
    deadline: Duration,
    poll_interval: Duration,
) -> Result<(), ActualError> {
    let creds = store::load()?.ok_or(ActualError::NotLoggedIn)?;
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

    // Resolve --repo to a repo id: a UUID is used as-is; any other value is a
    // repository name resolved via the connected-repos API. A 403 during that
    // lookup gets the same cross-org guidance as the query itself.
    let repo_unique_id = match args.repo.as_deref() {
        Some(value) => Some(
            resolve_repo(value, &client, &org_id)
                .await
                .map_err(|e| enrich_org_mismatch(e, &session_org, &org_id, explicit_org))?,
        ),
        None => None,
    };

    let request = AdvisorQueryRequest::new(
        org_id.clone(),
        repo_unique_id,
        args.query.clone(),
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
            query: "why app router?".to_string(),
            api_url: Some(api_url.to_string()),
            org: org.map(|s| s.to_string()),
            repo: None,
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
        run(&a, Duration::from_secs(60), Duration::ZERO)
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
        assert_eq!(resolve_repo(id, &client, "org").await.unwrap(), id);
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
        let id = resolve_repo("actual-cli", &client, "org").await.unwrap();
        assert_eq!(id, "33333333-3333-3333-3333-333333333333");
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
        run(&a, Duration::from_secs(60), Duration::ZERO)
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
        let err = run(&a, Duration::from_secs(60), Duration::ZERO)
            .await
            .unwrap_err();
        assert!(
            matches!(err, ActualError::RepoNotFound(ref m) if m.contains("ghost")),
            "got: {err:?}"
        );
    }
}
