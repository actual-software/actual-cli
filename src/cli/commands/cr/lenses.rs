use crate::cli::commands::cr::schemas::LENS_REVIEW_SCHEMA;
use crate::cli::commands::cr::types::{AdrContext, DiffContext, LensReview, ReviewerLens};
use crate::error::ActualError;
use crate::runner::subprocess::TailoringRunner;

impl ReviewerLens {
    /// All 8 reviewer lenses in canonical order.
    pub fn all() -> &'static [ReviewerLens] {
        &[
            ReviewerLens::PrincipalEngineer,
            ReviewerLens::ProgramManager,
            ReviewerLens::ProductManager,
            ReviewerLens::SecurityEngineer,
            ReviewerLens::PragmaticEngineer,
            ReviewerLens::OssMaintainer,
            ReviewerLens::TestEngineer,
            ReviewerLens::SreEngineer,
        ]
    }

    /// Human-readable name for this lens.
    pub fn name(&self) -> &'static str {
        match self {
            ReviewerLens::PrincipalEngineer => "Principal Engineer",
            ReviewerLens::ProgramManager => "Program Manager",
            ReviewerLens::ProductManager => "Product Manager",
            ReviewerLens::SecurityEngineer => "Security Engineer",
            ReviewerLens::PragmaticEngineer => "Pragmatic Engineer",
            ReviewerLens::OssMaintainer => "OSS Maintainer",
            ReviewerLens::TestEngineer => "Test Engineer",
            ReviewerLens::SreEngineer => "SRE Engineer",
        }
    }

    /// Domain focus area for this lens.
    pub fn domain(&self) -> &'static str {
        match self {
            ReviewerLens::PrincipalEngineer => "Architecture",
            ReviewerLens::ProgramManager => "Organizational",
            ReviewerLens::ProductManager => "Product Management",
            ReviewerLens::SecurityEngineer => "Security",
            ReviewerLens::PragmaticEngineer => "Engineering Trade-offs",
            ReviewerLens::OssMaintainer => "Open Source",
            ReviewerLens::TestEngineer => "Test Engineering / QA",
            ReviewerLens::SreEngineer => "SRE",
        }
    }

    /// What this lens reviews — used in the system prompt.
    pub fn description(&self) -> &'static str {
        match self {
            ReviewerLens::PrincipalEngineer => {
                "Structural integrity, abstraction boundaries, module coupling, \
                 pattern consistency with existing codebase"
            }
            ReviewerLens::ProgramManager => {
                "Cross-team dependencies, migration risks, release coordination, \
                 backward compatibility"
            }
            ReviewerLens::ProductManager => {
                "User-facing impact, feature completeness, UX consistency, accessibility"
            }
            ReviewerLens::SecurityEngineer => {
                "OWASP Top 10, input validation, secrets exposure, auth/authz, \
                 dependency vulnerabilities"
            }
            ReviewerLens::PragmaticEngineer => {
                "Over-engineering vs under-engineering, complexity budget, \
                 technical debt balance"
            }
            ReviewerLens::OssMaintainer => {
                "License compliance, public API stability, semver correctness, \
                 contributor experience"
            }
            ReviewerLens::TestEngineer => {
                "Test coverage for changed code, error path testing, regression risk, \
                 test quality"
            }
            ReviewerLens::SreEngineer => {
                "Deployment risk, observability, rollback capability, performance impact, \
                 resource consumption"
            }
        }
    }
}

/// Build the LLM prompt for a single lens review.
///
/// Follows the template from the architecture doc Section 5:
/// role description, ADR policies, diff stats, commit messages, full diff.
pub fn build_lens_prompt(lens: &ReviewerLens, diff: &DiffContext, adrs: &AdrContext) -> String {
    format!(
        "You are a {name} reviewing a code change.\n\
         \n\
         ## Your Role\n\
         {description}\n\
         \n\
         ## ADR Policies\n\
         {adr_content}\n\
         \n\
         ## Change Under Review\n\
         \n\
         ### Diff Statistics\n\
         {diff_stat}\n\
         \n\
         ### Commit Messages\n\
         {commit_messages}\n\
         \n\
         ### Full Diff\n\
         {diff_text}\n\
         \n\
         ## Instructions\n\
         Review this change ONLY from the perspective of {domain}.\n\
         \n\
         Signal definitions:\n\
         - Red: Unacceptable risk — high probability of significant harm if merged as-is.\n\
         - Yellow: Moderate risk — should be acknowledged and mitigated.\n\
         - Green: Low risk — well-structured, no significant concerns from this lens.\n\
         - Gray: Not applicable — this lens has no relevant perspective on the change.\n\
         \n\
         Reviewer conduct rules:\n\
         1. Be specific — cite file paths, line numbers, function names.\n\
         2. Be evidence-based — reference the diff or ADR policies.\n\
         3. Provide mitigation — every Yellow or Red finding MUST include a concrete suggested fix.\n\
         4. Stay in lane — only flag issues within {domain}.\n\
         5. Use Gray honestly — if the change has nothing relevant to this lens, say Gray.\n\
         6. Be proportional — match signal severity to actual risk.\n\
         \n\
         Produce a JSON response matching the provided schema.",
        name = lens.name(),
        description = lens.description(),
        adr_content = adrs.policies,
        diff_stat = diff.diff_stat,
        commit_messages = diff.commit_messages,
        diff_text = diff.diff_text,
        domain = lens.domain(),
    )
}

/// Run all lens reviews in parallel using `futures::future::join_all`.
///
/// Each lens gets an independent LLM call via `runner.run_raw_json()`.
/// All calls run concurrently; results are collected once all complete.
pub async fn run_parallel_reviews<R: TailoringRunner>(
    runner: &R,
    diff: &DiffContext,
    adrs: &AdrContext,
    lenses: &[ReviewerLens],
    model: Option<&str>,
    max_budget_usd: Option<f64>,
) -> Result<Vec<LensReview>, ActualError> {
    let futures: Vec<_> = lenses
        .iter()
        .map(|lens| {
            let prompt = build_lens_prompt(lens, diff, adrs);
            async move {
                let review: LensReview = runner
                    .run_raw_json(&prompt, LENS_REVIEW_SCHEMA, model, max_budget_usd)
                    .await?;
                Ok::<LensReview, ActualError>(review)
            }
        })
        .collect();

    let results = futures::future::join_all(futures).await;

    results.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::commands::cr::types::{AdrContext, DiffContext};

    #[test]
    fn test_all_lenses_count() {
        assert_eq!(ReviewerLens::all().len(), 8);
    }

    #[test]
    fn test_lens_names_are_non_empty() {
        for lens in ReviewerLens::all() {
            assert!(!lens.name().is_empty());
            assert!(!lens.domain().is_empty());
            assert!(!lens.description().is_empty());
        }
    }

    #[test]
    fn test_build_lens_prompt_contains_key_sections() {
        let diff = DiffContext {
            base_branch: "main".to_string(),
            head_sha: "abc123".to_string(),
            diff_text: "+fn foo() {}".to_string(),
            diff_stat: " 1 file changed, 1 insertion(+)".to_string(),
            commit_messages: "feat: add foo".to_string(),
            changed_files: vec!["src/lib.rs".to_string()],
        };
        let adrs = AdrContext {
            policies: "No unsafe code allowed.".to_string(),
            adr_sections: vec![],
            source_file: std::path::PathBuf::from("CLAUDE.md"),
        };

        let prompt = build_lens_prompt(&ReviewerLens::SecurityEngineer, &diff, &adrs);

        assert!(prompt.contains("Security Engineer"));
        assert!(prompt.contains("## Your Role"));
        assert!(prompt.contains("## ADR Policies"));
        assert!(prompt.contains("No unsafe code allowed."));
        assert!(prompt.contains("### Diff Statistics"));
        assert!(prompt.contains("### Commit Messages"));
        assert!(prompt.contains("### Full Diff"));
        assert!(prompt.contains("+fn foo() {}"));
        assert!(prompt.contains("Security"));
        assert!(prompt.contains("## Instructions"));
    }

    #[test]
    fn test_build_lens_prompt_each_lens_has_unique_role() {
        let diff = DiffContext {
            base_branch: "main".to_string(),
            head_sha: "abc123".to_string(),
            diff_text: "diff".to_string(),
            diff_stat: "stat".to_string(),
            commit_messages: "msgs".to_string(),
            changed_files: vec![],
        };
        let adrs = AdrContext {
            policies: "policies".to_string(),
            adr_sections: vec![],
            source_file: std::path::PathBuf::from("CLAUDE.md"),
        };

        let prompts: Vec<String> = ReviewerLens::all()
            .iter()
            .map(|lens| build_lens_prompt(lens, &diff, &adrs))
            .collect();

        // Each prompt should be unique (different role/domain)
        for i in 0..prompts.len() {
            for j in (i + 1)..prompts.len() {
                assert_ne!(prompts[i], prompts[j]);
            }
        }
    }
}
