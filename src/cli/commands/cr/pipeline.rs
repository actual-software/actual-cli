//! Pipeline orchestration for `actual cr` — runs phases 1–6 sequentially.

use std::path::PathBuf;

use crate::cli::args::CrArgs;
use crate::cli::commands::cr::types::*;
use crate::error::ActualError;
use crate::runner::subprocess::TailoringRunner;

use super::display;
use super::schemas::{CROSS_LENS_SYNTHESIS_SCHEMA, LENS_REVIEW_SCHEMA};

/// Run the full code review pipeline.
///
/// Orchestrates all 6 phases sequentially:
/// 1. Git diff gathering
/// 2. ADR reading
/// 3. Parallel lens reviews (8 LLM calls)
/// 4. Deterministic signal aggregation
/// 5. Cross-lens synthesis (1 LLM call)
/// 6. Display
///
/// Returns the process exit code (0 for Green/Yellow, 1 for Red, 2 for errors).
pub async fn run_review<R: TailoringRunner>(
    args: &CrArgs,
    runner: &R,
    runner_name: &str,
    effective_model: &str,
) -> Result<i32, ActualError> {
    // Phase 1: Git diff
    let diff = gather_diff(&args.base)?;
    if diff.diff_text.is_empty() {
        eprintln!("No changes to review between '{}' and HEAD.", args.base);
        return Ok(0);
    }

    // Phase 2: Read ADR context
    let adr_context = read_adr_context()?;

    // Phase 3: Parallel lens reviews
    let lenses = resolve_lenses(&args.lenses);
    let reviews = run_parallel_reviews(
        runner,
        &diff,
        &adr_context,
        &lenses,
        args.model.as_deref(),
        args.max_budget_usd,
    )
    .await?;

    // Phase 4: Deterministic signal aggregation
    let mut aggregated = aggregate_signals(&reviews);

    // Phase 5: Cross-lens synthesis (best-effort)
    match synthesize_cross_lens(
        runner,
        &reviews,
        &aggregated,
        &diff,
        args.model.as_deref(),
        args.max_budget_usd,
    )
    .await
    {
        Ok(synthesis) => {
            aggregated.cross_lens_synthesis = Some(synthesis);
        }
        Err(e) => {
            eprintln!("Warning: cross-lens synthesis failed: {e}");
            // Continue without synthesis — it's best-effort
        }
    }

    // Fill metadata
    aggregated.metadata = ReviewMetadata {
        timestamp_utc: chrono::Utc::now().to_rfc3339(),
        head_sha: diff.head_sha.clone(),
        base_branch: diff.base_branch.clone(),
        model: effective_model.to_string(),
        runner: runner_name.to_string(),
    };

    // Write review record
    if let Err(e) = write_review_record(&aggregated) {
        eprintln!("Warning: failed to write review record: {e}");
    }

    // Phase 6: Display
    display::print_review(&aggregated, args.json, args.verbose);

    // Exit code per policy
    let exit_code = match aggregated.overall_signal {
        Signal::Green | Signal::Yellow | Signal::Gray => 0,
        Signal::Red => 1,
    };

    Ok(exit_code)
}

// ── Phase 1: Git Diff ──

/// Gather the diff context between the base branch and HEAD.
fn gather_diff(base: &str) -> Result<DiffContext, ActualError> {
    let head_sha = run_git(&["rev-parse", "HEAD"])?;
    let diff_text = run_git(&["diff", &format!("{base}...HEAD")])?;
    let diff_stat = run_git(&["diff", &format!("{base}...HEAD"), "--stat"])?;
    let commit_messages = run_git(&["log", &format!("{base}..HEAD"), "--oneline"])?;
    let changed_files_raw = run_git(&["diff", &format!("{base}...HEAD"), "--name-only"])?;
    let changed_files: Vec<String> = changed_files_raw
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| l.to_string())
        .collect();

    Ok(DiffContext {
        base_branch: base.to_string(),
        head_sha: head_sha.trim().to_string(),
        diff_text,
        diff_stat,
        commit_messages,
        changed_files,
    })
}

/// Run a git command and return stdout.
fn run_git(args: &[&str]) -> Result<String, ActualError> {
    let output = std::process::Command::new("git")
        .args(args)
        .output()
        .map_err(|e| ActualError::CodeReviewError(format!("Failed to run git: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(ActualError::CodeReviewError(format!(
            "git {} failed: {}",
            args.join(" "),
            stderr.trim()
        )));
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

// ── Phase 2: ADR Reading ──

/// Read ADR-managed policy content from the project.
fn read_adr_context() -> Result<AdrContext, ActualError> {
    use crate::generation::markers::{extract_adr_sections, extract_managed_content};

    let cwd = std::env::current_dir()
        .map_err(|e| ActualError::CodeReviewError(format!("Failed to get cwd: {e}")))?;

    // Try common output files in order
    let candidates = ["CLAUDE.md", "AGENTS.md"];
    for name in &candidates {
        let path = cwd.join(name);
        if path.exists() {
            let content = std::fs::read_to_string(&path)
                .map_err(|e| ActualError::CodeReviewError(format!("Failed to read {name}: {e}")))?;

            let policies = extract_managed_content(&content)
                .unwrap_or(&content)
                .to_string();

            let sections = extract_adr_sections(&content);

            return Ok(AdrContext {
                policies,
                adr_sections: sections,
                source_file: path,
            });
        }
    }

    // No ADR files found — proceed with empty context
    Ok(AdrContext {
        policies: String::new(),
        adr_sections: vec![],
        source_file: PathBuf::new(),
    })
}

// ── Phase 3: Parallel Lens Reviews ──

/// Resolve which lenses to run based on the --lenses flag.
fn resolve_lenses(filter: &Option<Vec<String>>) -> Vec<ReviewerLens> {
    match filter {
        Some(names) => {
            let mut lenses = Vec::new();
            for name in names {
                if let Some(lens) = parse_lens_name(name) {
                    lenses.push(lens);
                } else {
                    eprintln!("Warning: unknown lens '{name}', skipping");
                }
            }
            if lenses.is_empty() {
                ReviewerLens::all().to_vec()
            } else {
                lenses
            }
        }
        None => ReviewerLens::all().to_vec(),
    }
}

/// Parse a lens name from the CLI (case-insensitive, short aliases).
fn parse_lens_name(name: &str) -> Option<ReviewerLens> {
    match name.to_ascii_lowercase().as_str() {
        "principal" | "principal_engineer" | "principal-engineer" => {
            Some(ReviewerLens::PrincipalEngineer)
        }
        "program" | "program_manager" | "program-manager" => Some(ReviewerLens::ProgramManager),
        "product" | "product_manager" | "product-manager" => Some(ReviewerLens::ProductManager),
        "security" | "security_engineer" | "security-engineer" => {
            Some(ReviewerLens::SecurityEngineer)
        }
        "pragmatic" | "pragmatic_engineer" | "pragmatic-engineer" => {
            Some(ReviewerLens::PragmaticEngineer)
        }
        "oss" | "oss_maintainer" | "oss-maintainer" => Some(ReviewerLens::OssMaintainer),
        "test" | "test_engineer" | "test-engineer" => Some(ReviewerLens::TestEngineer),
        "sre" | "sre_engineer" | "sre-engineer" => Some(ReviewerLens::SreEngineer),
        _ => None,
    }
}

/// Run all lens reviews in parallel using tokio::JoinSet.
async fn run_parallel_reviews<R: TailoringRunner>(
    runner: &R,
    diff: &DiffContext,
    adrs: &AdrContext,
    lenses: &[ReviewerLens],
    model: Option<&str>,
    max_budget_usd: Option<f64>,
) -> Result<Vec<LensReview>, ActualError> {
    let mut results: Vec<LensReview> = Vec::with_capacity(lenses.len());
    let mut failures = 0;

    // Run sequentially since TailoringRunner is borrowed (not 'static).
    // True parallel execution would require Arc<dyn TailoringRunner> or similar.
    for lens in lenses {
        let prompt = build_lens_prompt(lens, diff, adrs);
        match runner
            .run_raw_json::<LensReview>(&prompt, LENS_REVIEW_SCHEMA, model, max_budget_usd)
            .await
        {
            Ok(review) => results.push(review),
            Err(e) => {
                failures += 1;
                eprintln!("Warning: {} failed: {e}", lens.display_name());
                // Report as Gray with error note
                results.push(LensReview {
                    lens: *lens,
                    signal: Signal::Gray,
                    rationale: format!("Lens failed: {e}"),
                    findings: vec![],
                });
            }
        }
    }

    // Abort if too many lenses failed (3+)
    if failures >= 3 {
        return Err(ActualError::CodeReviewError(format!(
            "{failures} of {} lenses failed — aborting review",
            lenses.len()
        )));
    }

    Ok(results)
}

/// Build the prompt for a single lens review.
fn build_lens_prompt(lens: &ReviewerLens, diff: &DiffContext, adrs: &AdrContext) -> String {
    let role = match lens {
        ReviewerLens::PrincipalEngineer => {
            "You are a Principal Engineer reviewing code for architectural quality, design patterns, \
             complexity, and long-term maintainability."
        }
        ReviewerLens::ProgramManager => {
            "You are a Program Manager reviewing code for project scope, timeline risk, \
             cross-team dependencies, and delivery impact."
        }
        ReviewerLens::ProductManager => {
            "You are a Product Manager reviewing code for product alignment, user impact, \
             feature completeness, and business value."
        }
        ReviewerLens::SecurityEngineer => {
            "You are a Security Engineer reviewing code for vulnerabilities, input validation, \
             authentication, authorization, and data protection."
        }
        ReviewerLens::PragmaticEngineer => {
            "You are a Pragmatic Engineer reviewing code for over-engineering, unnecessary \
             abstractions, premature optimization, and proportionality of the solution."
        }
        ReviewerLens::OssMaintainer => {
            "You are an OSS Maintainer reviewing code for public API surface, backward compatibility, \
             documentation quality, and community impact."
        }
        ReviewerLens::TestEngineer => {
            "You are a Test Engineer reviewing code for test coverage, test quality, \
             edge case handling, and testability of the design."
        }
        ReviewerLens::SreEngineer => {
            "You are an SRE Engineer reviewing code for deployment risk, observability, \
             performance regression, resource utilization, and operational impact."
        }
    };

    let adr_section = if adrs.policies.is_empty() {
        "No ADR policies found for this project.".to_string()
    } else {
        format!("## ADR Policies\n\n{}", adrs.policies)
    };

    format!(
        "{role}\n\n\
         {adr_section}\n\n\
         ## Change Under Review\n\n\
         Base branch: {base}\n\
         Commits:\n{commits}\n\n\
         Diff stats:\n{stats}\n\n\
         Changed files: {files}\n\n\
         ## Full Diff\n\n\
         ```\n{diff}\n```\n\n\
         ## Instructions\n\n\
         Review this change through your lens. Produce a JSON object with:\n\
         - `lens`: your lens identifier (snake_case)\n\
         - `signal`: \"red\", \"yellow\", \"green\", or \"gray\"\n\
         - `rationale`: one-sentence summary of your signal\n\
         - `findings`: array of specific findings (may be empty for green/gray)\n\n\
         Each finding needs: severity, summary, details (evidence from the diff), \
         suggestion (concrete fix), related_adrs (if any), affected_files.",
        base = diff.base_branch,
        commits = diff.commit_messages,
        stats = diff.diff_stat,
        files = diff.changed_files.join(", "),
        diff = diff.diff_text,
    )
}

// ── Phase 4: Deterministic Signal Aggregation ──

/// Aggregate per-lens signals into an overall result.
fn aggregate_signals(reviews: &[LensReview]) -> AggregatedResult {
    let non_gray: Vec<&LensReview> = reviews
        .iter()
        .filter(|r| r.signal != Signal::Gray)
        .collect();

    // Role-based auto-block: Security or SRE Red → overall Red
    let security_red = reviews
        .iter()
        .any(|r| r.lens == ReviewerLens::SecurityEngineer && r.signal == Signal::Red);
    let sre_red = reviews
        .iter()
        .any(|r| r.lens == ReviewerLens::SreEngineer && r.signal == Signal::Red);

    // 3+ Yellow escalation → Red
    let yellow_count = non_gray
        .iter()
        .filter(|r| r.signal == Signal::Yellow)
        .count();

    let any_red = non_gray.iter().any(|r| r.signal == Signal::Red);

    let overall = if security_red || sre_red || any_red || yellow_count >= 3 {
        Signal::Red
    } else if non_gray.iter().any(|r| r.signal == Signal::Yellow) {
        Signal::Yellow
    } else {
        Signal::Green
    };

    let decision = Decision::from_signal(overall);

    AggregatedResult {
        per_lens: reviews.to_vec(),
        overall_signal: overall,
        decision,
        cross_lens_synthesis: None,
        metadata: ReviewMetadata {
            timestamp_utc: String::new(),
            head_sha: String::new(),
            base_branch: String::new(),
            model: String::new(),
            runner: String::new(),
        },
    }
}

// ── Phase 5: Cross-Lens Synthesis ──

/// Run the cross-lens synthesis LLM call.
async fn synthesize_cross_lens<R: TailoringRunner>(
    runner: &R,
    reviews: &[LensReview],
    aggregated: &AggregatedResult,
    diff: &DiffContext,
    model: Option<&str>,
    max_budget_usd: Option<f64>,
) -> Result<CrossLensSynthesis, ActualError> {
    let prompt = build_synthesis_prompt(reviews, aggregated, diff);
    runner
        .run_raw_json::<CrossLensSynthesis>(
            &prompt,
            CROSS_LENS_SYNTHESIS_SCHEMA,
            model,
            max_budget_usd,
        )
        .await
}

/// Build the synthesis prompt from all lens reviews.
fn build_synthesis_prompt(
    reviews: &[LensReview],
    aggregated: &AggregatedResult,
    diff: &DiffContext,
) -> String {
    let reviews_json = serde_json::to_string_pretty(reviews).unwrap_or_else(|_| "[]".to_string());

    format!(
        "You are a cross-functional code review synthesizer.\n\n\
         ## Per-Lens Reviews\n\n\
         ```json\n{reviews_json}\n```\n\n\
         ## Aggregated Result\n\n\
         Overall signal: {overall}\n\
         Decision: {decision}\n\n\
         ## Diff Statistics\n\n\
         {stats}\n\n\
         ## Instructions\n\n\
         Analyze all 8 lens reviews together. Produce a JSON object with:\n\
         - `convergent`: risks flagged by 3+ lenses (patterns of agreement)\n\
         - `divergent`: where lenses disagree (one praises, another flags)\n\
         - `surprising`: unique insights from a single lens that others missed\n\
         - `most_relevant`: top 3 findings most directly applicable to the changes\n\n\
         Each item needs: lenses (array of lens names), summary, action.",
        overall = aggregated.overall_signal.display(),
        decision = aggregated.decision.display(),
        stats = diff.diff_stat,
    )
}

// ── Phase 6 utilities ──

/// Write review record to .actual/cr-reviews/ as timestamped JSON.
fn write_review_record(result: &AggregatedResult) -> Result<(), ActualError> {
    let dir = PathBuf::from(".actual/cr-reviews");
    std::fs::create_dir_all(&dir)
        .map_err(|e| ActualError::CodeReviewError(format!("Failed to create review dir: {e}")))?;

    let filename = format!("{}.json", result.metadata.timestamp_utc.replace(':', "-"));
    let path = dir.join(filename);

    let json = serde_json::to_string_pretty(result)
        .map_err(|e| ActualError::CodeReviewError(format!("Failed to serialize review: {e}")))?;

    std::fs::write(&path, json)
        .map_err(|e| ActualError::CodeReviewError(format!("Failed to write review record: {e}")))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aggregate_all_green() {
        let reviews = vec![
            LensReview {
                lens: ReviewerLens::PrincipalEngineer,
                signal: Signal::Green,
                rationale: "OK".to_string(),
                findings: vec![],
            },
            LensReview {
                lens: ReviewerLens::SecurityEngineer,
                signal: Signal::Green,
                rationale: "OK".to_string(),
                findings: vec![],
            },
        ];
        let result = aggregate_signals(&reviews);
        assert_eq!(result.overall_signal, Signal::Green);
        assert_eq!(result.decision, Decision::Proceed);
    }

    #[test]
    fn aggregate_security_red_escalates() {
        let reviews = vec![
            LensReview {
                lens: ReviewerLens::PrincipalEngineer,
                signal: Signal::Green,
                rationale: "OK".to_string(),
                findings: vec![],
            },
            LensReview {
                lens: ReviewerLens::SecurityEngineer,
                signal: Signal::Red,
                rationale: "Critical vuln".to_string(),
                findings: vec![],
            },
        ];
        let result = aggregate_signals(&reviews);
        assert_eq!(result.overall_signal, Signal::Red);
        assert_eq!(result.decision, Decision::BlockOrEscalate);
    }

    #[test]
    fn aggregate_sre_red_escalates() {
        let reviews = vec![LensReview {
            lens: ReviewerLens::SreEngineer,
            signal: Signal::Red,
            rationale: "Deployment risk".to_string(),
            findings: vec![],
        }];
        let result = aggregate_signals(&reviews);
        assert_eq!(result.overall_signal, Signal::Red);
    }

    #[test]
    fn aggregate_single_yellow() {
        let reviews = vec![
            LensReview {
                lens: ReviewerLens::TestEngineer,
                signal: Signal::Yellow,
                rationale: "Missing tests".to_string(),
                findings: vec![],
            },
            LensReview {
                lens: ReviewerLens::PrincipalEngineer,
                signal: Signal::Green,
                rationale: "OK".to_string(),
                findings: vec![],
            },
        ];
        let result = aggregate_signals(&reviews);
        assert_eq!(result.overall_signal, Signal::Yellow);
        assert_eq!(result.decision, Decision::ProceedWithMitigation);
    }

    #[test]
    fn aggregate_three_yellows_escalates_to_red() {
        let reviews = vec![
            LensReview {
                lens: ReviewerLens::PrincipalEngineer,
                signal: Signal::Yellow,
                rationale: "Concern 1".to_string(),
                findings: vec![],
            },
            LensReview {
                lens: ReviewerLens::TestEngineer,
                signal: Signal::Yellow,
                rationale: "Concern 2".to_string(),
                findings: vec![],
            },
            LensReview {
                lens: ReviewerLens::PragmaticEngineer,
                signal: Signal::Yellow,
                rationale: "Concern 3".to_string(),
                findings: vec![],
            },
        ];
        let result = aggregate_signals(&reviews);
        assert_eq!(result.overall_signal, Signal::Red);
        assert_eq!(result.decision, Decision::BlockOrEscalate);
    }

    #[test]
    fn aggregate_gray_ignored() {
        let reviews = vec![
            LensReview {
                lens: ReviewerLens::OssMaintainer,
                signal: Signal::Gray,
                rationale: "N/A".to_string(),
                findings: vec![],
            },
            LensReview {
                lens: ReviewerLens::PrincipalEngineer,
                signal: Signal::Green,
                rationale: "OK".to_string(),
                findings: vec![],
            },
        ];
        let result = aggregate_signals(&reviews);
        assert_eq!(result.overall_signal, Signal::Green);
    }

    #[test]
    fn resolve_lenses_all_by_default() {
        let lenses = resolve_lenses(&None);
        assert_eq!(lenses.len(), 8);
    }

    #[test]
    fn resolve_lenses_filter() {
        let lenses = resolve_lenses(&Some(vec!["security".to_string(), "sre".to_string()]));
        assert_eq!(lenses.len(), 2);
        assert_eq!(lenses[0], ReviewerLens::SecurityEngineer);
        assert_eq!(lenses[1], ReviewerLens::SreEngineer);
    }

    #[test]
    fn resolve_lenses_unknown_skipped() {
        let lenses = resolve_lenses(&Some(vec!["unknown".to_string()]));
        // Falls back to all when filter produces empty
        assert_eq!(lenses.len(), 8);
    }

    #[test]
    fn parse_lens_name_aliases() {
        assert_eq!(
            parse_lens_name("security"),
            Some(ReviewerLens::SecurityEngineer)
        );
        assert_eq!(
            parse_lens_name("security_engineer"),
            Some(ReviewerLens::SecurityEngineer)
        );
        assert_eq!(
            parse_lens_name("security-engineer"),
            Some(ReviewerLens::SecurityEngineer)
        );
        assert_eq!(
            parse_lens_name("SECURITY"),
            Some(ReviewerLens::SecurityEngineer)
        );
        assert_eq!(parse_lens_name("sre"), Some(ReviewerLens::SreEngineer));
        assert_eq!(parse_lens_name("test"), Some(ReviewerLens::TestEngineer));
        assert_eq!(parse_lens_name("oss"), Some(ReviewerLens::OssMaintainer));
        assert_eq!(parse_lens_name("bogus"), None);
    }

    #[test]
    fn build_lens_prompt_contains_diff() {
        let diff = DiffContext {
            base_branch: "main".to_string(),
            head_sha: "abc123".to_string(),
            diff_text: "+added line".to_string(),
            diff_stat: "1 file changed".to_string(),
            commit_messages: "feat: add thing".to_string(),
            changed_files: vec!["src/lib.rs".to_string()],
        };
        let adrs = AdrContext {
            policies: "Security: validate inputs".to_string(),
            adr_sections: vec![],
            source_file: PathBuf::from("CLAUDE.md"),
        };
        let prompt = build_lens_prompt(&ReviewerLens::SecurityEngineer, &diff, &adrs);
        assert!(prompt.contains("Security Engineer"));
        assert!(prompt.contains("+added line"));
        assert!(prompt.contains("validate inputs"));
    }

    #[test]
    fn build_synthesis_prompt_contains_reviews() {
        let reviews = vec![LensReview {
            lens: ReviewerLens::PrincipalEngineer,
            signal: Signal::Green,
            rationale: "OK".to_string(),
            findings: vec![],
        }];
        let aggregated = aggregate_signals(&reviews);
        let diff = DiffContext {
            base_branch: "main".to_string(),
            head_sha: "abc123".to_string(),
            diff_text: String::new(),
            diff_stat: "1 file changed".to_string(),
            commit_messages: String::new(),
            changed_files: vec![],
        };
        let prompt = build_synthesis_prompt(&reviews, &aggregated, &diff);
        assert!(prompt.contains("principal_engineer"));
        assert!(prompt.contains("convergent"));
    }
}
