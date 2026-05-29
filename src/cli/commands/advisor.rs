//! `actual advisor <query>` — ask the Advisor org-scoped architecture questions.
//!
//! Starts an async advisor job, polls to completion (honoring the server's
//! `Retry-After`), and renders the answer. Uses the platform token from
//! `actual login` as the bearer.

use std::time::Duration;

use crate::api::types::{
    AdvisorJobStatus, AdvisorOutput, AdvisorPoll, AdvisorQueryRequest, AdvisorSink, AdvisorSurface,
};
use crate::api::{ActualApiClient, DEFAULT_API_URL};
use crate::auth::store;
use crate::cli::args::AdvisorArgs;
use crate::cli::ui::theme;
use crate::error::ActualError;

/// Upper bound on poll attempts before giving up (guards against a backend
/// that never reaches a terminal state).
const MAX_POLL_ATTEMPTS: usize = 600;
/// Default delay between polls when the server provides no `Retry-After`.
const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(2);

pub fn exec(args: &AdvisorArgs) -> Result<(), ActualError> {
    build_runtime()?.block_on(run(args, MAX_POLL_ATTEMPTS, DEFAULT_POLL_INTERVAL))
}

/// Single-threaded tokio runtime so the sync CLI dispatch path can drive the
/// async query flow (mirrors `commands::login`).
fn build_runtime() -> Result<tokio::runtime::Runtime, ActualError> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| ActualError::InternalError(format!("failed to build tokio runtime: {e}")))
}

/// Resolve the Advisor API base URL: `--api-url` wins, else the api-service.
fn resolve_api_url(flag: Option<&str>) -> String {
    flag.map(|s| s.to_string())
        .unwrap_or_else(|| DEFAULT_API_URL.to_string())
}

/// Terminal outcome of an advisor job.
enum Outcome {
    Succeeded(Box<AdvisorOutput>),
    Failed(Option<String>),
}

async fn run(
    args: &AdvisorArgs,
    max_attempts: usize,
    poll_interval: Duration,
) -> Result<(), ActualError> {
    let creds = store::load()?.ok_or(ActualError::NotLoggedIn)?;
    let org_id = args
        .org
        .clone()
        .unwrap_or_else(|| creds.organization_id.clone());
    let base_url = resolve_api_url(args.api_url.as_deref());
    let client = ActualApiClient::new(&base_url)?.with_bearer(&creds.access_token);

    let request = AdvisorQueryRequest {
        org_id,
        repo_unique_id: None, // repo auto-scoping lands in a later slice
        query: args.query.clone(),
        surface: AdvisorSurface::cli(),
        sink: AdvisorSink::None,
        idempotency_key: None,
    };

    let started = client.start_advisor_query(&request).await?;
    eprintln!("{} thinking…", theme::hint("advisor"));
    let outcome =
        poll_to_completion(&client, &started.query_id, max_attempts, poll_interval).await?;

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

/// Poll the job until it reaches a terminal state (or `max_attempts` is hit).
async fn poll_to_completion(
    client: &ActualApiClient,
    query_id: &str,
    max_attempts: usize,
    poll_interval: Duration,
) -> Result<Outcome, ActualError> {
    for _ in 0..max_attempts {
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
        }
    }
    Err(ActualError::ApiError(
        "Advisor query did not reach a result in time.".to_string(),
    ))
}

/// Sleep for the server's `Retry-After` seconds, or the default interval.
async fn sleep_for(retry_after: Option<u64>, default: Duration) {
    let delay = retry_after.map(Duration::from_secs).unwrap_or(default);
    tokio::time::sleep(delay).await;
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
        }
    }

    #[test]
    fn test_resolve_api_url() {
        assert_eq!(
            resolve_api_url(Some("http://localhost:3099")),
            "http://localhost:3099"
        );
        assert_eq!(resolve_api_url(None), DEFAULT_API_URL);
    }

    #[test]
    fn test_print_answer_with_and_without_adrs() {
        let with = AdvisorOutput {
            summary: "S".to_string(),
            interpreter: crate::api::types::AdvisorInterpreter {
                summary: "i".to_string(),
                related_adrs: vec![crate::api::types::RelatedAdr {
                    id: "a".to_string(),
                    name: "n".to_string(),
                    title: "T".to_string(),
                    policy: "p".to_string(),
                    instructions: "i".to_string(),
                    scope: "s".to_string(),
                    relevance_reason: "r".to_string(),
                    confidence: 0.5,
                }],
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
        let err = run(&args("http://127.0.0.1:1", None), 5, Duration::ZERO)
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
        run(&args(&server.url(), None), 5, Duration::ZERO)
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
            5,
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
        let err = run(&args(&server.url(), None), 5, Duration::ZERO)
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
        let err = run(&args(&server.url(), None), 5, Duration::ZERO)
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
            5,
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
        run(&args(&server.url(), None), 5, Duration::ZERO)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn test_poll_exhausts_attempts() {
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
        // Always running → the loop exhausts max_attempts.
        let _p = server
            .mock("GET", POLL_PATH)
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_header("retry-after", "0")
            .with_body(r#"{"query_id":"q1","status":"running","result":null,"error":null}"#)
            .create_async()
            .await;
        let err = run(&args(&server.url(), None), 2, Duration::ZERO)
            .await
            .unwrap_err();
        assert!(matches!(err, ActualError::ApiError(ref m) if m.contains("did not reach")));
    }
}
