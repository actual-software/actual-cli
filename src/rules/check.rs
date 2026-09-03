//! The conformance judge: does a plan actually obey the rules that govern it?
//!
//! # Design
//!
//! [`super::scope`] answers a different question — *which* committed rule
//! documents apply to a plan. This module answers the one that matters for
//! gating an `ExitPlanMode` call: for each individual rule inside those
//! documents, does the plan conform, conflict, or deliberately change the
//! decision the rule encodes?
//!
//! That third outcome is why this is not a boolean. A plan that says "we are
//! moving off RS256 to Ed25519, superseding R-014" is not the same failure
//! mode as a plan that quietly does the same thing with no acknowledgment that
//! R-014 exists — the first is a decision, the second is an oversight. Only
//! the second should ever block a human's ExitPlanMode dialog; the first is
//! flagged for review, not denied. See [`Verdict`].
//!
//! **One call, not one per rule.** The candidate set (already capped by
//! [`super::scope::select`]) is batched into a single structured-output
//! request, the same shape [`super::scope::rank`] uses for document selection.
//! A synchronous `PreToolUse` hook has one interactive latency budget to spend,
//! and fanning out per-rule calls would spend it on network round trips
//! instead of on the judgement itself.
//!
//! **Every non-conforming verdict names the rule id and quotes the plan.** A
//! deny that only says "a rule was violated" is not actionable — the whole
//! point of gating at the plan stage rather than after implementation is that
//! the agent can revise immediately, which requires knowing *which* rule and
//! *what in the plan* triggered it. When the model's own answer omits the
//! quote, [`parse_verdicts`] falls back to the rule's statement rather than
//! leaving the field empty, the same way [`super::scope::select`] falls back
//! to its own evidence when a rank's reason is blank.
//!
//! **It may only judge, never invent or drop silently.** A verdict naming a
//! rule id that was never a candidate is dropped. A rule the model did not
//! judge at all stays unjudged rather than defaulting to `conforming` — an
//! absence of information must never manufacture a pass, but for the same
//! reason it must never manufacture a block either: the caller treats an
//! entirely unusable response as "could not check," not as a denial.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::error::ActualError;
use crate::rules::types::RuleLevel;
use crate::runner::structured::StructuredRunner;

/// The wall-clock budget for one conformance check.
///
/// `actual plan-check --claude-hook` runs inside Claude Code's 120-second
/// `PreToolUse` timeout, and selection's own stage-2 rank is skipped for this
/// path precisely so the one model call this module makes has the budget to
/// itself (see `crate::cli::commands::plan_check`). This deadline is on the
/// whole call, not on inactivity between streamed events — the same reasoning
/// [`super::scope::rank::RANK_BUDGET`] documents — so a backend that keeps
/// talking past it still degrades to fail-open rather than blocking the hook.
pub const CHECK_BUDGET: Duration = Duration::from_secs(90);

/// How the judge classified a rule against the plan.
///
/// Three values, not a boolean, because the middle one is the reason this
/// module exists: a plan that knowingly supersedes a decision must never be
/// blocked the same way one that silently contradicts it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// The plan does not contradict this rule.
    Conforming,
    /// The plan does something this rule forbids, or omits something it
    /// requires, with no sign the change was deliberate. This is the only
    /// verdict a caller should ever deny on.
    Conflicting,
    /// The plan explicitly changes the decision this rule encodes — a
    /// supersession, not an oversight. Flagged for human review rather than
    /// denied: routing it into a draft Decision is out of this MVP's scope.
    RequiresDecision,
}

impl Verdict {
    pub fn as_str(self) -> &'static str {
        match self {
            Verdict::Conforming => "conforming",
            Verdict::Conflicting => "conflicting",
            Verdict::RequiresDecision => "requires_decision",
        }
    }

    /// Parse a verdict word, case-insensitively, tolerating `-` for `_`.
    fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().replace('-', "_").as_str() {
            "conforming" => Some(Verdict::Conforming),
            "conflicting" => Some(Verdict::Conflicting),
            "requires_decision" => Some(Verdict::RequiresDecision),
            _ => None,
        }
    }

    /// True for the one verdict that may ever gate the tool call.
    pub fn blocks(self) -> bool {
        matches!(self, Verdict::Conflicting)
    }
}

/// One rule as shown to the judge: enough to decide conformance, without the
/// surrounding document's title, scope prose, or verify block, none of which
/// bear on whether *this plan* obeys *this statement*.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RuleForJudging {
    /// The document this rule came from, carried through to the result so a
    /// caller can point back at the file even though the judge is not shown it.
    pub doc_slug: String,
    pub rule_id: String,
    pub level: RuleLevel,
    pub statement: String,
}

impl RuleForJudging {
    pub fn new(
        doc_slug: impl Into<String>,
        rule_id: impl Into<String>,
        level: RuleLevel,
        statement: impl Into<String>,
    ) -> Self {
        Self {
            doc_slug: doc_slug.into(),
            rule_id: rule_id.into(),
            level,
            statement: statement.into(),
        }
    }
}

/// One rule's verdict, validated against the candidate set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CheckedRule {
    pub doc_slug: String,
    pub rule_id: String,
    pub level: RuleLevel,
    pub statement: String,
    pub verdict: Verdict,
    /// The exact plan text that conflicts. Always populated for
    /// [`Verdict::Conflicting`] and [`Verdict::RequiresDecision`] — falling
    /// back to the rule's own statement when the model's answer omitted a
    /// quote — and empty for [`Verdict::Conforming`].
    pub span: String,
    pub reason: String,
}

/// The JSON schema the judge's structured output must satisfy.
pub const CHECK_OUTPUT_SCHEMA: &str = r#"{
  "type": "object",
  "properties": {
    "verdicts": {
      "type": "array",
      "description": "One entry per rule, in any order.",
      "items": {
        "type": "object",
        "properties": {
          "rule_id": {
            "type": "string",
            "description": "The rule's id, copied exactly."
          },
          "verdict": {
            "type": "string",
            "enum": ["conforming", "conflicting", "requires_decision"],
            "description": "conforming: the plan does not contradict the rule. conflicting: the plan violates the rule and this looks unintentional. requires_decision: the plan deliberately supersedes the decision the rule encodes."
          },
          "span": {
            "type": "string",
            "description": "The exact, verbatim span of the plan that conflicts. Empty string for conforming."
          },
          "reason": {
            "type": "string",
            "description": "One sentence naming the rule id and what in the plan it applies to."
          }
        },
        "required": ["rule_id", "verdict", "span", "reason"],
        "additionalProperties": false
      }
    }
  },
  "required": ["verdicts"],
  "additionalProperties": false
}"#;

/// Build the judge's prompt.
///
/// Pure and stable: the same plan and the same rule list in the same order
/// produce the same bytes.
pub fn build_prompt(plan: &str, rules: &[RuleForJudging]) -> String {
    let mut out = String::new();
    out.push_str(
        "A developer wrote an implementation plan. Decide, for each rule below, whether \
         the plan conforms to it, conflicts with it, or deliberately changes the decision \
         it encodes.\n\n=== plan ===\n",
    );
    out.push_str(plan.trim());
    out.push('\n');

    out.push_str("\n=== rules ===\n");
    for rule in rules {
        out.push_str("\n- id: ");
        out.push_str(&rule.rule_id);
        out.push('\n');
        out.push_str("  level: ");
        out.push_str(rule.level.as_str());
        out.push('\n');
        out.push_str("  statement: ");
        out.push_str(&rule.statement);
        out.push('\n');
    }

    out.push_str(
        "\n=== task ===\n\
         Return one verdict for every rule above, copying each id exactly.\n\
         - `conforming`: the plan does not contradict this rule.\n\
         - `conflicting`: the plan does something this rule forbids, or omits something it \
         requires, and nothing in the plan suggests this was intentional.\n\
         - `requires_decision`: the plan explicitly and deliberately changes the decision \
         this rule encodes (a stated supersession), rather than merely overlooking it.\n\
         For every rule you judge `conflicting` or `requires_decision`, quote the exact span \
         of the plan, verbatim, that conflicts. Judge only the rules listed. Do not invent a \
         rule id and do not omit one.\n\
         Give a one-sentence reason for each, naming the rule id and what in the plan it \
         applies to.\n",
    );
    out
}

/// Why a check could not be used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckError {
    /// The runner returned something that is not the agreed shape, or judged
    /// none of the candidate rules at all.
    Malformed(String),
}

impl std::fmt::Display for CheckError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CheckError::Malformed(detail) => write!(f, "unusable check output: {detail}"),
        }
    }
}

/// Validate a judge's raw output against the rules it was given.
///
/// Everything the model could get wrong is handled here rather than trusted:
/// a rule id that was never a candidate is dropped, a repeated id keeps its
/// first verdict, an unrecognized verdict word leaves that rule unjudged, and
/// a rule the model skipped simply stays unjudged. A conflicting or
/// requires-decision verdict with an empty span falls back to the rule's own
/// statement, so the "names the span" contract holds regardless of what the
/// model actually returned.
pub fn parse_verdicts(
    value: &serde_json::Value,
    candidates: &[RuleForJudging],
) -> Result<Vec<CheckedRule>, CheckError> {
    let entries = value
        .get("verdicts")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| CheckError::Malformed("no `verdicts` array in the response".to_string()))?;

    let mut out: Vec<CheckedRule> = Vec::new();
    for entry in entries {
        let Some(rule_id) = entry.get("rule_id").and_then(serde_json::Value::as_str) else {
            continue;
        };
        let rule_id = rule_id.trim();
        let Some(candidate) = candidates.iter().find(|c| c.rule_id == rule_id) else {
            tracing::debug!(
                rule_id,
                "check named a rule id that was not a candidate; dropping it"
            );
            continue;
        };
        if out.iter().any(|v| v.rule_id == rule_id) {
            continue;
        }
        let Some(verdict) = entry
            .get("verdict")
            .and_then(serde_json::Value::as_str)
            .and_then(Verdict::parse)
        else {
            continue;
        };
        let reason = entry
            .get("reason")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        let span = entry
            .get("span")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        let span = if verdict.blocks() || matches!(verdict, Verdict::RequiresDecision) {
            if span.is_empty() {
                candidate.statement.clone()
            } else {
                span
            }
        } else {
            String::new()
        };
        out.push(CheckedRule {
            doc_slug: candidate.doc_slug.clone(),
            rule_id: candidate.rule_id.clone(),
            level: candidate.level,
            statement: candidate.statement.clone(),
            verdict,
            span,
            reason,
        });
    }

    if out.is_empty() {
        return Err(CheckError::Malformed(
            "no rule in the candidate set received a verdict".to_string(),
        ));
    }
    Ok(out)
}

/// Ask `runner` to judge `rules` against `plan`, inside [`CHECK_BUDGET`].
///
/// Returns the validated verdicts, or the reason the check could not be used.
/// A transport failure, an overrun budget, and an unusable answer all come
/// back as an error the caller can catch and fail open on — a conformance
/// check that could not run is never grounds to block a plan.
pub async fn check<R: StructuredRunner>(
    runner: &R,
    plan: &str,
    rules: &[RuleForJudging],
    model_override: Option<&str>,
    max_budget_usd: Option<f64>,
) -> Result<Vec<CheckedRule>, ActualError> {
    let prompt = build_prompt(plan, rules);
    let call =
        runner.run_structured_json(&prompt, CHECK_OUTPUT_SCHEMA, model_override, max_budget_usd);
    let value = tokio::time::timeout(CHECK_BUDGET, call)
        .await
        .map_err(|_| ActualError::RunnerTimeout {
            seconds: CHECK_BUDGET.as_secs(),
        })??;
    parse_verdicts(&value, rules).map_err(|e| ActualError::TailoringValidationError(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules() -> Vec<RuleForJudging> {
        vec![
            RuleForJudging::new(
                "cross-cutting-token-signing-1c57",
                "R-A-001",
                RuleLevel::Must,
                "sign access tokens with RS256.",
            ),
            RuleForJudging::new(
                "cross-cutting-token-signing-1c57",
                "R-A-002",
                RuleLevel::MustNot,
                "log the raw signing key.",
            ),
        ]
    }

    fn verdicts_value(entries: serde_json::Value) -> serde_json::Value {
        serde_json::json!({ "verdicts": entries })
    }

    #[test]
    fn test_schema_is_valid_json_with_the_agreed_shape() {
        let schema: serde_json::Value = serde_json::from_str(CHECK_OUTPUT_SCHEMA).unwrap();
        assert_eq!(schema["type"], "object");
        assert_eq!(schema["required"][0], "verdicts");
        let item = &schema["properties"]["verdicts"]["items"];
        assert_eq!(item["additionalProperties"], false);
        let required: Vec<&str> = item["required"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(required, vec!["rule_id", "verdict", "span", "reason"]);
    }

    #[test]
    fn test_verdict_words_round_trip() {
        for verdict in [
            Verdict::Conforming,
            Verdict::Conflicting,
            Verdict::RequiresDecision,
        ] {
            assert_eq!(Verdict::parse(verdict.as_str()), Some(verdict));
        }
        assert_eq!(
            Verdict::parse("  REQUIRES-DECISION "),
            Some(Verdict::RequiresDecision)
        );
        assert_eq!(Verdict::parse("maybe"), None);
    }

    #[test]
    fn test_only_conflicting_blocks() {
        assert!(!Verdict::Conforming.blocks());
        assert!(Verdict::Conflicting.blocks());
        assert!(!Verdict::RequiresDecision.blocks());
    }

    #[test]
    fn test_build_prompt_includes_plan_and_every_rule() {
        let prompt = build_prompt("Add a Redis cache.", &rules());
        assert!(prompt.contains("Add a Redis cache."));
        assert!(prompt.contains("R-A-001"));
        assert!(prompt.contains("R-A-002"));
        assert!(prompt.contains("sign access tokens with RS256."));
    }

    #[test]
    fn test_build_prompt_is_deterministic() {
        let rules = rules();
        assert_eq!(
            build_prompt("plan text", &rules),
            build_prompt("plan text", &rules)
        );
    }

    #[test]
    fn test_parse_verdicts_happy_path() {
        let value = verdicts_value(serde_json::json!([
            {"rule_id": "R-A-001", "verdict": "conforming", "span": "", "reason": "uses RS256 as required"},
            {"rule_id": "R-A-002", "verdict": "conflicting", "span": "log the signing key to stdout for debugging", "reason": "R-A-002 forbids logging the key"},
        ]));
        let verdicts = parse_verdicts(&value, &rules()).unwrap();
        assert_eq!(verdicts.len(), 2);
        assert_eq!(verdicts[0].rule_id, "R-A-001");
        assert_eq!(verdicts[0].verdict, Verdict::Conforming);
        assert_eq!(verdicts[0].span, "");
        assert_eq!(verdicts[1].verdict, Verdict::Conflicting);
        assert_eq!(
            verdicts[1].span,
            "log the signing key to stdout for debugging"
        );
    }

    #[test]
    fn test_parse_verdicts_drops_a_rule_id_that_was_not_a_candidate() {
        let value = verdicts_value(serde_json::json!([
            {"rule_id": "R-A-001", "verdict": "conforming", "span": "", "reason": "ok"},
            {"rule_id": "R-GHOST-999", "verdict": "conflicting", "span": "x", "reason": "hallucinated"},
        ]));
        let verdicts = parse_verdicts(&value, &rules()).unwrap();
        assert_eq!(verdicts.len(), 1);
        assert_eq!(verdicts[0].rule_id, "R-A-001");
    }

    #[test]
    fn test_parse_verdicts_keeps_first_verdict_on_a_repeated_id() {
        let value = verdicts_value(serde_json::json!([
            {"rule_id": "R-A-001", "verdict": "conforming", "span": "", "reason": "first"},
            {"rule_id": "R-A-001", "verdict": "conflicting", "span": "x", "reason": "second"},
        ]));
        let verdicts = parse_verdicts(&value, &rules()).unwrap();
        assert_eq!(verdicts.len(), 1);
        assert_eq!(verdicts[0].verdict, Verdict::Conforming);
    }

    #[test]
    fn test_parse_verdicts_leaves_an_unrecognized_verdict_word_unjudged() {
        let value = verdicts_value(serde_json::json!([
            {"rule_id": "R-A-001", "verdict": "maybe", "span": "", "reason": "unsure"},
            {"rule_id": "R-A-002", "verdict": "conforming", "span": "", "reason": "fine"},
        ]));
        let verdicts = parse_verdicts(&value, &rules()).unwrap();
        assert_eq!(verdicts.len(), 1);
        assert_eq!(verdicts[0].rule_id, "R-A-002");
    }

    /// The acceptance criterion that matters most: a non-conforming verdict
    /// must always carry a span, even when the model's own answer left it
    /// blank.
    #[test]
    fn test_parse_verdicts_falls_back_to_the_statement_when_span_is_blank() {
        let value = verdicts_value(serde_json::json!([
            {"rule_id": "R-A-002", "verdict": "conflicting", "span": "  ", "reason": "violates it"},
        ]));
        let verdicts = parse_verdicts(&value, &rules()).unwrap();
        assert_eq!(verdicts[0].span, "log the raw signing key.");
    }

    #[test]
    fn test_parse_verdicts_requires_decision_also_falls_back_to_the_statement() {
        let value = verdicts_value(serde_json::json!([
            {"rule_id": "R-A-001", "verdict": "requires_decision", "span": "", "reason": "supersedes it"},
        ]));
        let verdicts = parse_verdicts(&value, &rules()).unwrap();
        assert_eq!(verdicts[0].span, "sign access tokens with RS256.");
    }

    #[test]
    fn test_parse_verdicts_errors_without_a_verdicts_array() {
        let value = serde_json::json!({ "not_verdicts": [] });
        let err = parse_verdicts(&value, &rules()).unwrap_err();
        assert!(matches!(err, CheckError::Malformed(_)));
    }

    #[test]
    fn test_parse_verdicts_errors_when_nothing_was_judged() {
        let value = verdicts_value(serde_json::json!([]));
        let err = parse_verdicts(&value, &rules()).unwrap_err();
        assert!(matches!(err, CheckError::Malformed(_)));
    }

    #[test]
    fn test_parse_verdicts_errors_when_every_entry_names_an_unknown_id() {
        let value = verdicts_value(serde_json::json!([
            {"rule_id": "R-GHOST", "verdict": "conforming", "span": "", "reason": "n/a"},
        ]));
        let err = parse_verdicts(&value, &rules()).unwrap_err();
        assert!(matches!(err, CheckError::Malformed(_)));
    }

    #[test]
    fn test_check_error_display() {
        let err = CheckError::Malformed("no array".to_string());
        assert_eq!(err.to_string(), "unusable check output: no array");
    }

    // ── async check() ────────────────────────────────────────────────────

    struct FakeRunner {
        response: serde_json::Value,
    }

    impl StructuredRunner for FakeRunner {
        async fn run_structured_json(
            &self,
            _prompt: &str,
            _schema: &str,
            _model_override: Option<&str>,
            _max_budget_usd: Option<f64>,
        ) -> Result<serde_json::Value, ActualError> {
            Ok(self.response.clone())
        }
    }

    #[tokio::test]
    async fn test_check_returns_parsed_verdicts_on_success() {
        let runner = FakeRunner {
            response: verdicts_value(serde_json::json!([
                {"rule_id": "R-A-001", "verdict": "conforming", "span": "", "reason": "ok"},
                {"rule_id": "R-A-002", "verdict": "conforming", "span": "", "reason": "ok"},
            ])),
        };
        let verdicts = check(&runner, "a plan", &rules(), None, None)
            .await
            .unwrap();
        assert_eq!(verdicts.len(), 2);
        assert!(verdicts.iter().all(|v| v.verdict == Verdict::Conforming));
    }

    #[tokio::test]
    async fn test_check_errors_on_a_malformed_response() {
        let runner = FakeRunner {
            response: serde_json::json!({ "nonsense": true }),
        };
        let err = check(&runner, "a plan", &rules(), None, None)
            .await
            .unwrap_err();
        assert!(matches!(err, ActualError::TailoringValidationError(_)));
    }

    struct TimeoutRunner;

    impl StructuredRunner for TimeoutRunner {
        async fn run_structured_json(
            &self,
            _prompt: &str,
            _schema: &str,
            _model_override: Option<&str>,
            _max_budget_usd: Option<f64>,
        ) -> Result<serde_json::Value, ActualError> {
            std::future::pending::<()>().await;
            unreachable!()
        }
    }

    #[tokio::test(start_paused = true)]
    async fn test_check_times_out_rather_than_hanging_forever() {
        let err = check(&TimeoutRunner, "a plan", &rules(), None, None)
            .await
            .unwrap_err();
        assert!(matches!(err, ActualError::RunnerTimeout { .. }));
    }
}
