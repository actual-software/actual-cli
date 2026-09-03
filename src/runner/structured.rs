//! One structured invocation, independent of what the schema describes.
//!
//! # Design
//!
//! [`TailoringRunner`](crate::runner::TailoringRunner) names its return type,
//! so every runner that implements it can answer exactly one question. Stage-2
//! rule selection asks a different one against the same five backends, and the
//! honest way to reach them is not a second parallel set of implementations but
//! the same bodies with the output type lifted out.
//!
//! Each runner therefore keeps one private `run_structured::<T>` holding the
//! plumbing it already had — subprocess flags, retry, tool-use extraction — and
//! both traits are thin calls into it. `run_tailoring` still returns
//! `TailoringOutput` and behaves identically; this trait returns
//! [`serde_json::Value`] and lets the caller decide what the bytes mean.
//!
//! `Value` rather than a second generic method is deliberate. A generic trait
//! method would make the trait unusable behind a `dyn`, and every caller here
//! validates the payload against its own rules anyway — a rank naming a rule
//! that was never a candidate has to be dropped, not deserialized.

use crate::error::ActualError;

/// A runner that can answer any schema, not only the tailoring one.
///
/// Implemented by all five backends. The caller supplies the prompt and the
/// JSON schema and gets the model's structured answer back unparsed.
pub trait StructuredRunner: Send + Sync {
    /// Run `prompt` against `schema` and return the structured result.
    ///
    /// `model_override` and `max_budget_usd` carry the same meaning they do for
    /// tailoring: each runner applies whichever of them its backend supports.
    fn run_structured_json(
        &self,
        prompt: &str,
        schema: &str,
        model_override: Option<&str>,
        max_budget_usd: Option<f64>,
    ) -> impl std::future::Future<Output = Result<serde_json::Value, ActualError>> + Send;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A trivial implementation, asserting the trait is implementable outside
    /// the runner modules — which is what lets a test fake stand in for a real
    /// backend without a subprocess or a socket.
    struct EchoRunner;

    impl StructuredRunner for EchoRunner {
        async fn run_structured_json(
            &self,
            prompt: &str,
            _schema: &str,
            model_override: Option<&str>,
            _max_budget_usd: Option<f64>,
        ) -> Result<serde_json::Value, ActualError> {
            Ok(serde_json::json!({
                "prompt": prompt,
                "model": model_override,
            }))
        }
    }

    #[tokio::test]
    async fn test_a_fake_runner_satisfies_the_trait() {
        let value = EchoRunner
            .run_structured_json("plan", "{}", Some("haiku"), None)
            .await
            .unwrap();
        assert_eq!(value["prompt"], "plan");
        assert_eq!(value["model"], "haiku");
    }
}
