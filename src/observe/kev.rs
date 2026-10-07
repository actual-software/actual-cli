use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::env;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

const DEFAULT_KEV_URL: &str = "http://localhost:8009";
const DEFAULT_TIMEOUT_MS: u64 = 2000;

#[derive(Debug)]
pub enum KevError {
    Unavailable(String),
}

#[derive(Debug, Clone, Deserialize)]
pub struct NoulResponse {
    pub probability: f64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ChoiceResponse {
    pub selected: String,
    pub distribution: HashMap<String, f64>,
    pub confidence: f64,
}

#[derive(Debug, Clone, Serialize)]
struct NoulQuestion {
    r#type: String,
    instructions: String,
}

#[derive(Debug, Clone, Serialize)]
struct ChoiceQuestion {
    r#type: String,
    instructions: String,
    criteria: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize)]
struct SystemOneRequest<Q: Serialize> {
    state: String,
    questions: HashMap<String, Q>,
}

#[derive(Debug, Clone, Deserialize)]
struct NoulAnswer {
    noul: f64,
}

#[derive(Debug, Clone, Deserialize)]
struct ChoiceAnswer {
    choice: String,
    probabilities: HashMap<String, f64>,
    confidence: f64,
}

#[derive(Debug, Clone, Deserialize)]
struct SystemOneResponse<A> {
    answers: HashMap<String, A>,
}

pub fn kev_url() -> String {
    env::var("KEV_URL").unwrap_or_else(|_| DEFAULT_KEV_URL.to_string())
}

/// Per-request timeout, from `KEV_TIMEOUT_MS` (default 2000). The model's first call after
/// idling can take several seconds; a demo or a cold host can raise this instead of falling back.
pub fn kev_timeout() -> Duration {
    env::var("KEV_TIMEOUT_MS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|ms| *ms > 0)
        .map(Duration::from_millis)
        .unwrap_or(Duration::from_millis(DEFAULT_TIMEOUT_MS))
}

/// `KEV_URL=off` (or disabled/0/false) turns kev off; hooks then use the deterministic rules only.
fn kev_disabled(url: &str) -> bool {
    matches!(
        url.trim().to_ascii_lowercase().as_str(),
        "off" | "disabled" | "0" | "false" | ""
    )
}

pub struct KevClient {
    client: Client,
    base_url: String,
    /// `call_id` of the most recent request, so the boundary's decision record can point at it.
    last_call_id: Mutex<Option<String>>,
    /// Where `call`/`decision` records go; resolved once at construction.
    log_path: Option<PathBuf>,
    /// Model, latency and answer of the most recent call, for the decision signal.
    last_call: Mutex<Option<serde_json::Value>>,
    /// The most recent decision this client drove (or fell back on). The hook attaches it to
    /// the event so the Advisor request (step 2) carries why step 1 gated the action.
    last_signal: Mutex<Option<serde_json::Value>>,
}

/// Audit log for kev: `~/.actual/logs/kev.jsonl` (override `KEV_LOG_PATH`, disable `KEV_LOG=off`).
/// One `call` record per request (exact input, raw output, latency) and one `decision` record
/// per boundary check that used it.
pub fn kev_log_path() -> Option<PathBuf> {
    if env::var("KEV_LOG")
        .map(|v| kev_disabled(&v))
        .unwrap_or(false)
    {
        return None;
    }
    if let Ok(p) = env::var("KEV_LOG_PATH") {
        if !p.trim().is_empty() {
            return Some(PathBuf::from(p));
        }
    }
    Some(
        dirs::home_dir()?
            .join(".actual")
            .join("logs")
            .join("kev.jsonl"),
    )
}

fn append_log(path: Option<&PathBuf>, mut record: serde_json::Value) {
    let Some(path) = path else { return };
    if let Some(obj) = record.as_object_mut() {
        obj.insert(
            "ts".into(),
            serde_json::json!(chrono::Utc::now().to_rfc3339()),
        );
    }
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    // Best effort: an unwritable log must never break a hook. One write per line so
    // concurrent hook processes cannot interleave records.
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = f.write_all(format!("{record}\n").as_bytes());
    }
}

impl KevClient {
    pub fn new() -> Option<Self> {
        Self::with_base_url(&kev_url(), kev_timeout())
    }

    /// The hook path's constructor: `None` when `KEV_URL` turns kev off.
    pub fn from_env() -> Option<Self> {
        let url = kev_url();
        if kev_disabled(&url) {
            return None;
        }
        Self::with_base_url(&url, kev_timeout())
    }

    pub fn with_base_url(base_url: &str, timeout: Duration) -> Option<Self> {
        let client = Client::builder()
            .timeout(timeout)
            // A dead or unrouted host must not stall a hook for the whole timeout.
            .connect_timeout(timeout.min(Duration::from_millis(500)))
            .build()
            .ok()?;
        Some(Self {
            client,
            base_url: base_url.trim_end_matches('/').to_string(),
            last_call_id: Mutex::new(None),
            last_call: Mutex::new(None),
            last_signal: Mutex::new(None),
            // Unit tests never write to the real ~/.actual/logs; they opt in with with_log_path.
            log_path: if cfg!(test) { None } else { kev_log_path() },
        })
    }

    /// Send this client's audit records to `path` (`None` disables them).
    pub fn with_log_path(mut self, path: Option<PathBuf>) -> Self {
        self.log_path = path;
        self
    }

    /// One request to `/v1/systemone`, logged as a `call` record: exact input, HTTP status,
    /// raw response (or error), latency.
    async fn post<Q: Serialize>(
        &self,
        kind: &str,
        req: &SystemOneRequest<Q>,
    ) -> Result<serde_json::Value, KevError> {
        let call_id = uuid::Uuid::new_v4().to_string();
        if let Ok(mut last) = self.last_call_id.lock() {
            *last = Some(call_id.clone());
        }
        let url = format!("{}/v1/systemone", self.base_url);
        let started = Instant::now();
        let outcome: Result<(u16, serde_json::Value), String> = async {
            let resp = self
                .client
                .post(&url)
                .json(req)
                .send()
                .await
                .map_err(|e| e.to_string())?;
            let status = resp.status().as_u16();
            let text = resp.text().await.map_err(|e| e.to_string())?;
            let body = serde_json::from_str(&text).unwrap_or(serde_json::Value::String(text));
            Ok((status, body))
        }
        .await;
        let latency_ms = started.elapsed().as_millis() as u64;
        let request = serde_json::to_value(req).unwrap_or(serde_json::Value::Null);
        let mut record = serde_json::json!({
            "record": "call", "call_id": call_id, "kind": kind, "url": url,
            "latency_ms": latency_ms, "request": request,
        });
        let result = match outcome {
            Ok((status, body)) => {
                record["http_status"] = serde_json::json!(status);
                record["response"] = body.clone();
                if (200..300).contains(&status) {
                    Ok(body)
                } else {
                    let e = format!("kev returned HTTP {status}");
                    record["error"] = serde_json::json!(e);
                    Err(KevError::Unavailable(e))
                }
            }
            Err(e) => {
                record["error"] = serde_json::json!(e);
                Err(KevError::Unavailable(e))
            }
        };
        if let Ok(mut last) = self.last_call.lock() {
            *last = Some(serde_json::json!({
                "call_id": call_id,
                "model": record.get("response").and_then(|r| r.get("model")).cloned(),
                "latency_ms": latency_ms,
                "answer": record.get("response").and_then(|r| r.pointer("/answers/q")).cloned(),
            }));
        }
        tracing::debug!(target: "kev", %call_id, latency_ms, request = %record["request"], response = %record.get("response").unwrap_or(&serde_json::Value::Null), "kev call");
        append_log(self.log_path.as_ref(), record);
        result
    }

    /// Records what a boundary check decided with (or without) kev, linked to the last call.
    #[allow(clippy::too_many_arguments)]
    pub fn log_decision(
        &self,
        check: &str,
        score_name: &str,
        score: Option<f64>,
        threshold: f64,
        verdict: bool,
        source: &str,
        error: Option<&str>,
    ) {
        let call_id = self.last_call_id.lock().ok().and_then(|g| g.clone());
        let decision = serde_json::json!({
            "call_id": call_id, "check": check,
            "score_name": score_name, "score": score, "threshold": threshold,
            "verdict": verdict, "source": source, "error": error,
        });
        let mut signal = decision.clone();
        if let Some(call) = self.last_call.lock().ok().and_then(|g| g.clone()) {
            for key in ["model", "latency_ms", "answer"] {
                signal[key] = call.get(key).cloned().unwrap_or(serde_json::Value::Null);
            }
        }
        if let Ok(mut last) = self.last_signal.lock() {
            *last = Some(signal);
        }
        let mut record = decision;
        record["record"] = serde_json::json!("decision");
        append_log(self.log_path.as_ref(), record);
    }

    /// The decision kev drove for this hook, if it was consulted: check, score, threshold,
    /// verdict, source (`kev` or `fallback_rules`), model, latency and kev's raw answer.
    pub fn last_signal(&self) -> Option<serde_json::Value> {
        self.last_signal.lock().ok().and_then(|g| g.clone())
    }

    pub async fn noul(&self, state: &str, instructions: &str) -> Result<NoulResponse, KevError> {
        let mut questions = HashMap::new();
        questions.insert(
            "q".to_string(),
            NoulQuestion {
                r#type: "noul".to_string(),
                instructions: instructions.to_string(),
            },
        );
        let req = SystemOneRequest {
            state: state.to_string(),
            questions,
        };
        let raw = self.post("noul", &req).await?;
        let body: SystemOneResponse<NoulAnswer> = serde_json::from_value(raw)
            .map_err(|e| KevError::Unavailable(format!("bad kev response: {e}")))?;

        let answer = body
            .answers
            .get("q")
            .ok_or_else(|| KevError::Unavailable("missing answer key 'q'".into()))?;

        Ok(NoulResponse {
            probability: answer.noul,
        })
    }

    pub async fn choice(
        &self,
        state: &str,
        instructions: &str,
        options: &[(&str, &str)],
    ) -> Result<ChoiceResponse, KevError> {
        let criteria: HashMap<String, String> = options
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        let mut questions = HashMap::new();
        questions.insert(
            "q".to_string(),
            ChoiceQuestion {
                r#type: "choice".to_string(),
                instructions: instructions.to_string(),
                criteria,
            },
        );
        let req = SystemOneRequest {
            state: state.to_string(),
            questions,
        };
        let raw = self.post("choice", &req).await?;
        let body: SystemOneResponse<ChoiceAnswer> = serde_json::from_value(raw)
            .map_err(|e| KevError::Unavailable(format!("bad kev response: {e}")))?;

        let answer = body
            .answers
            .get("q")
            .ok_or_else(|| KevError::Unavailable("missing answer key 'q'".into()))?;

        Ok(ChoiceResponse {
            selected: answer.choice.clone(),
            distribution: answer.probabilities.clone(),
            confidence: answer.confidence,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── RED 1: choice request JSON shape ──

    #[test]
    fn test_choice_request_serialization() {
        let criteria: HashMap<String, String> = [
            ("read_only".to_string(), "Read-only operation".to_string()),
            ("mutating".to_string(), "Mutating operation".to_string()),
        ]
        .into();
        let mut questions = HashMap::new();
        questions.insert(
            "q".to_string(),
            ChoiceQuestion {
                r#type: "choice".to_string(),
                instructions: "Classify this command".to_string(),
                criteria,
            },
        );
        let req = SystemOneRequest {
            state: "ls -la".to_string(),
            questions,
        };
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["state"], "ls -la");
        assert_eq!(json["questions"]["q"]["type"], "choice");
        assert!(json["questions"]["q"]["criteria"]["read_only"].is_string());
    }

    // ── RED 2: noul request JSON shape ──

    #[test]
    fn test_noul_request_serialization() {
        let mut questions = HashMap::new();
        questions.insert(
            "q".to_string(),
            NoulQuestion {
                r#type: "noul".to_string(),
                instructions: "Is this mutating?".to_string(),
            },
        );
        let req = SystemOneRequest {
            state: "rm -rf dist/".to_string(),
            questions,
        };
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["state"], "rm -rf dist/");
        assert_eq!(json["questions"]["q"]["type"], "noul");
        assert_eq!(json["questions"]["q"]["instructions"], "Is this mutating?");
    }

    // ── RED 3: kev_url reads env, defaults ──

    #[test]
    fn test_kev_url_default() {
        let _lock = crate::testutil::ENV_MUTEX
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _guard = crate::testutil::EnvGuard::remove("KEV_URL");
        assert_eq!(kev_url(), "http://localhost:8009");
    }

    #[test]
    fn test_kev_url_from_env() {
        let _lock = crate::testutil::ENV_MUTEX
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _guard = crate::testutil::EnvGuard::set("KEV_URL", "http://kev.internal:9000");
        assert_eq!(kev_url(), "http://kev.internal:9000");
    }

    // ── RED 4: noul response deserialization ──

    #[test]
    fn test_noul_response_deserialization() {
        let json = r#"{
            "answers": {
                "q": { "type": "noul", "noul": 0.87 }
            },
            "model": "kev-0.8b",
            "usage": {"prompt_tokens": 42, "completion_tokens": 0, "total_tokens": 42},
            "request_id": "test-001",
            "latency_ms": 12
        }"#;
        let resp: SystemOneResponse<NoulAnswer> = serde_json::from_str(json).unwrap();
        let answer = resp.answers.get("q").unwrap();
        assert!((answer.noul - 0.87).abs() < 1e-6);
    }

    // ── RED 5: choice response deserialization ──

    #[test]
    fn test_choice_response_deserialization() {
        let json = r#"{
            "answers": {
                "q": {
                    "type": "choice",
                    "choice": "mutating",
                    "confidence": 0.92,
                    "probabilities": { "read_only": 0.08, "mutating": 0.92 }
                }
            },
            "model": "kev-0.8b",
            "usage": {"prompt_tokens": 55, "completion_tokens": 0, "total_tokens": 55},
            "request_id": "test-002",
            "latency_ms": 15
        }"#;
        let resp: SystemOneResponse<ChoiceAnswer> = serde_json::from_str(json).unwrap();
        let answer = resp.answers.get("q").unwrap();
        assert_eq!(answer.choice, "mutating");
        assert!((answer.confidence - 0.92).abs() < 1e-6);
    }

    // ── Live: real kev server at KEV_URL (default :8009). Run with
    //    `cargo test --lib observe::kev::tests::live -- --ignored --test-threads=1` ──

    #[tokio::test]
    #[ignore = "needs a running kev server (KEV_URL, default http://localhost:8009)"]
    async fn live_noul_returns_a_probability() {
        let kev = KevClient::new().expect("client");
        let resp = kev
            .noul(
                "Refactor the payment module to use ts-rest contracts",
                "Is this a substantive coding instruction that requires architectural review?",
            )
            .await
            .unwrap_or_else(|KevError::Unavailable(e)| {
                panic!("kev unavailable at {}: {e}", kev_url())
            });
        assert!(
            (0.0..=1.0).contains(&resp.probability),
            "probability {}",
            resp.probability
        );
    }

    #[tokio::test]
    #[ignore = "needs a running kev server (KEV_URL, default http://localhost:8009)"]
    async fn live_choice_classifies_a_mutating_command() {
        let kev = KevClient::new().expect("client");
        let resp = kev
            .choice(
                "git add -A && git commit -m wip | tee /tmp/log; rm -rf dist",
                "Classify this bash command's filesystem/repository impact",
                &[
                    (
                        "read_only",
                        "Only reads data, lists files, or queries state",
                    ),
                    (
                        "mutating",
                        "Modifies files, repository, packages, or infrastructure",
                    ),
                    ("ambiguous", "Could be either depending on flags or context"),
                ],
            )
            .await
            .unwrap_or_else(|KevError::Unavailable(e)| {
                panic!("kev unavailable at {}: {e}", kev_url())
            });
        assert_eq!(resp.selected, "mutating");
        let total: f64 = resp.distribution.values().sum();
        assert!((total - 1.0).abs() < 0.01, "distribution sums to {total}");
    }

    #[tokio::test]
    #[ignore = "needs a running kev server (KEV_URL, default http://localhost:8009)"]
    async fn live_boundary_v2_uses_kev_for_complex_commands() {
        let kev = KevClient::new().expect("client");
        // Complex (pipe) and missed by the deterministic check, so the decision comes from kev.
        let payload = serde_json::json!({"tool_name": "Bash", "tool_input": {"command": "cat package.json | jq '.version = \"2.0.0\"' | sponge package.json"}});
        assert!(super::super::boundary::is_mutating_bash_v2(&payload, Some(&kev)).await);
        let read_only = serde_json::json!({"tool_name": "Bash", "tool_input": {"command": "git log --oneline | head -20 | grep fix"}});
        assert!(!super::super::boundary::is_mutating_bash_v2(&read_only, Some(&kev)).await);
    }

    // ── Hook-path construction from env ──

    #[test]
    fn from_env_is_disabled_by_kev_url_off() {
        let _lock = crate::testutil::ENV_MUTEX
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        for off in ["off", "OFF", "disabled", "0", "false"] {
            let _guard = crate::testutil::EnvGuard::set("KEV_URL", off);
            assert!(
                KevClient::from_env().is_none(),
                "KEV_URL={off} should disable kev"
            );
        }
    }

    #[test]
    fn timeout_reads_kev_timeout_ms() {
        let _lock = crate::testutil::ENV_MUTEX
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _unset = crate::testutil::EnvGuard::remove("KEV_TIMEOUT_MS");
        assert_eq!(kev_timeout(), Duration::from_millis(2000));
        let _set = crate::testutil::EnvGuard::set("KEV_TIMEOUT_MS", "6000");
        assert_eq!(kev_timeout(), Duration::from_millis(6000));
        let _bad = crate::testutil::EnvGuard::set("KEV_TIMEOUT_MS", "nope");
        assert_eq!(kev_timeout(), Duration::from_millis(2000));
    }
}
