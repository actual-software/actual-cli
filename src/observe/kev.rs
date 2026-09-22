use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::env;
use std::time::Duration;

const DEFAULT_KEV_URL: &str = "http://localhost:8787";
const TIMEOUT_SECS: u64 = 2;

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

pub struct KevClient {
    client: Client,
    base_url: String,
}

impl KevClient {
    pub fn new() -> Option<Self> {
        let client = Client::builder()
            .timeout(Duration::from_secs(TIMEOUT_SECS))
            .build()
            .ok()?;
        Some(Self {
            client,
            base_url: kev_url(),
        })
    }

    pub async fn noul(
        &self,
        state: &str,
        instructions: &str,
    ) -> Result<NoulResponse, KevError> {
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
        let resp = self
            .client
            .post(format!("{}/v1/systemone", self.base_url))
            .json(&req)
            .send()
            .await
            .map_err(|e| KevError::Unavailable(e.to_string()))?;

        let body: SystemOneResponse<NoulAnswer> = resp
            .json()
            .await
            .map_err(|e| KevError::Unavailable(e.to_string()))?;

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
        let resp = self
            .client
            .post(format!("{}/v1/systemone", self.base_url))
            .json(&req)
            .send()
            .await
            .map_err(|e| KevError::Unavailable(e.to_string()))?;

        let body: SystemOneResponse<ChoiceAnswer> = resp
            .json()
            .await
            .map_err(|e| KevError::Unavailable(e.to_string()))?;

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
        let _lock = crate::testutil::ENV_MUTEX.lock().unwrap();
        let _guard = crate::testutil::EnvGuard::remove("KEV_URL");
        assert_eq!(kev_url(), "http://localhost:8787");
    }

    #[test]
    fn test_kev_url_from_env() {
        let _lock = crate::testutil::ENV_MUTEX.lock().unwrap();
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
}
