use crate::cli::commands::cr::schemas::CROSS_LENS_SYNTHESIS_SCHEMA;
use crate::cli::commands::cr::types::{
    AggregatedResult, CrossLensSynthesis, DiffContext, LensReview,
};
use crate::error::ActualError;
use crate::runner::subprocess::TailoringRunner;

/// Build the prompt for the 9th LLM call: cross-lens synthesis.
///
/// Given all 8 lens reviews and the deterministic aggregation result,
/// asks the LLM to identify cross-lens patterns: convergent, divergent,
/// surprising, and most relevant findings.
pub fn build_synthesis_prompt(
    reviews: &[LensReview],
    aggregated: &AggregatedResult,
    diff: &DiffContext,
) -> String {
    let serialized_reviews =
        serde_json::to_string_pretty(reviews).unwrap_or_else(|_| "[]".to_string());

    format!(
        "You are a meta-reviewer synthesizing 8 independent code review perspectives.\n\
         \n\
         ## Per-Lens Reviews\n\
         {serialized_reviews}\n\
         \n\
         ## Aggregated Result\n\
         Overall signal: {overall_signal:?}\n\
         Decision: {decision:?}\n\
         \n\
         ## Diff Statistics\n\
         {diff_stat}\n\
         \n\
         ## Instructions\n\
         Analyze the 8 reviews for cross-lens patterns. Produce a JSON response:\n\
         - convergent: findings flagged by 3+ lenses (cite which lenses agree)\n\
         - divergent: findings where lenses disagree (one Green, another Yellow/Red)\n\
         - surprising: unique insights from exactly one lens\n\
         - most_relevant: top 3 findings most applicable to THIS specific change\n\
         \n\
         If a category has no findings, return an empty array for it.\n\
         \n\
         Produce a JSON response matching the provided schema.",
        serialized_reviews = serialized_reviews,
        overall_signal = aggregated.overall_signal,
        decision = aggregated.decision,
        diff_stat = diff.diff_stat,
    )
}

/// Run the cross-lens synthesis (the 9th LLM call).
///
/// Takes all lens reviews and the deterministic aggregation result,
/// then asks the LLM to identify patterns across lenses.
pub async fn synthesize_cross_lens<R: TailoringRunner>(
    runner: &R,
    reviews: &[LensReview],
    aggregated: &AggregatedResult,
    diff: &DiffContext,
    model: Option<&str>,
    max_budget_usd: Option<f64>,
    max_tokens: Option<u32>,
) -> Result<CrossLensSynthesis, ActualError> {
    let prompt = build_synthesis_prompt(reviews, aggregated, diff);
    let schema = CROSS_LENS_SYNTHESIS_SCHEMA;
    runner
        .run_raw_json(&prompt, schema, model, max_budget_usd, max_tokens)
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::commands::cr::types::{
        Decision, DiffContext, LensReview, ReviewMetadata, ReviewerLens, Signal,
    };

    fn sample_reviews() -> Vec<LensReview> {
        vec![
            LensReview {
                lens: ReviewerLens::SecurityEngineer,
                signal: Signal::Yellow,
                rationale: "Missing input validation on /api/login".to_string(),
                findings: vec![],
            },
            LensReview {
                lens: ReviewerLens::TestEngineer,
                signal: Signal::Yellow,
                rationale: "No tests for error paths".to_string(),
                findings: vec![],
            },
            LensReview {
                lens: ReviewerLens::PrincipalEngineer,
                signal: Signal::Green,
                rationale: "Architecture is sound".to_string(),
                findings: vec![],
            },
        ]
    }

    fn sample_aggregated(reviews: &[LensReview]) -> AggregatedResult {
        AggregatedResult {
            per_lens: reviews.to_vec(),
            overall_signal: Signal::Yellow,
            decision: Decision::ProceedWithMitigation,
            cross_lens_synthesis: None,
            metadata: ReviewMetadata {
                timestamp_utc: "2026-03-09T00:00:00Z".to_string(),
                head_sha: "abc123".to_string(),
                base_branch: "main".to_string(),
                model: "claude-sonnet-4-6".to_string(),
                runner: "anthropic-api".to_string(),
            },
        }
    }

    fn sample_diff() -> DiffContext {
        DiffContext {
            base_branch: "main".to_string(),
            head_sha: "abc123".to_string(),
            diff_text: "+fn login() {}".to_string(),
            diff_stat: " 3 files changed, 42 insertions(+), 5 deletions(-)".to_string(),
            commit_messages: "feat: add login endpoint".to_string(),
            changed_files: vec!["src/auth.rs".to_string()],
        }
    }

    #[test]
    fn test_build_synthesis_prompt_contains_key_sections() {
        let reviews = sample_reviews();
        let aggregated = sample_aggregated(&reviews);
        let diff = sample_diff();

        let prompt = build_synthesis_prompt(&reviews, &aggregated, &diff);

        assert!(prompt.contains("meta-reviewer"));
        assert!(prompt.contains("## Per-Lens Reviews"));
        assert!(prompt.contains("security_engineer"));
        assert!(prompt.contains("## Aggregated Result"));
        assert!(prompt.contains("Yellow"));
        assert!(prompt.contains("## Diff Statistics"));
        assert!(prompt.contains("3 files changed"));
        assert!(prompt.contains("convergent"));
        assert!(prompt.contains("divergent"));
        assert!(prompt.contains("surprising"));
        assert!(prompt.contains("most_relevant"));
    }

    #[test]
    fn test_build_synthesis_prompt_serializes_reviews_as_json() {
        let reviews = sample_reviews();
        let aggregated = sample_aggregated(&reviews);
        let diff = sample_diff();

        let prompt = build_synthesis_prompt(&reviews, &aggregated, &diff);

        // The reviews should be valid JSON embedded in the prompt
        assert!(prompt.contains("\"lens\": \"security_engineer\""));
        assert!(prompt.contains("\"signal\": \"yellow\""));
        assert!(prompt.contains("\"lens\": \"principal_engineer\""));
        assert!(prompt.contains("\"signal\": \"green\""));
    }
}
