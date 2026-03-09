use serde::{Deserialize, Serialize};

/// The 8 reviewer lenses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewerLens {
    PrincipalEngineer,
    ProgramManager,
    ProductManager,
    SecurityEngineer,
    PragmaticEngineer,
    OssMaintainer,
    TestEngineer,
    SreEngineer,
}

impl ReviewerLens {
    /// Human-readable name for display in the signal matrix.
    pub fn display_name(&self) -> &'static str {
        match self {
            Self::PrincipalEngineer => "Principal Engineer",
            Self::ProgramManager => "Program Manager",
            Self::ProductManager => "Product Manager",
            Self::SecurityEngineer => "Security Engineer",
            Self::PragmaticEngineer => "Pragmatic Engineer",
            Self::OssMaintainer => "OSS Maintainer",
            Self::TestEngineer => "Test Engineer",
            Self::SreEngineer => "SRE Engineer",
        }
    }

    /// All 8 lenses in canonical order.
    pub fn all() -> &'static [ReviewerLens] {
        &[
            Self::PrincipalEngineer,
            Self::ProgramManager,
            Self::ProductManager,
            Self::SecurityEngineer,
            Self::PragmaticEngineer,
            Self::OssMaintainer,
            Self::TestEngineer,
            Self::SreEngineer,
        ]
    }
}

/// Risk signal levels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Signal {
    Gray,
    Green,
    Yellow,
    Red,
}

impl Signal {
    /// Short icon label for the signal matrix.
    pub fn icon(&self) -> &'static str {
        match self {
            Self::Red => "RED",
            Self::Yellow => "YLW",
            Self::Green => "GRN",
            Self::Gray => "N/A",
        }
    }

    /// Human-readable display string.
    pub fn display(&self) -> &'static str {
        match self {
            Self::Red => "Red",
            Self::Yellow => "Yellow",
            Self::Green => "Green",
            Self::Gray => "Gray",
        }
    }
}

/// A single finding within a lens review.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Finding {
    /// Severity of this specific finding
    pub severity: Signal,
    /// ADR IDs related to this finding (empty if none)
    pub related_adrs: Vec<String>,
    /// Files affected by this finding
    pub affected_files: Vec<String>,
    /// One-line summary
    pub summary: String,
    /// Detailed explanation with evidence from the diff
    pub details: String,
    /// Concrete suggested fix or mitigation
    pub suggestion: String,
}

/// Complete review output from one lens.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LensReview {
    /// Which lens produced this review
    pub lens: ReviewerLens,
    /// Overall signal for this lens
    pub signal: Signal,
    /// One-sentence rationale for the signal
    pub rationale: String,
    /// Individual findings (may be empty for Green/Gray)
    pub findings: Vec<Finding>,
}

/// Cross-lens synthesis from the 9th LLM call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrossLensSynthesis {
    /// Risks flagged by 3+ lenses
    pub convergent: Vec<SynthesisFinding>,
    /// Where lenses disagree (one praises, another flags)
    pub divergent: Vec<SynthesisFinding>,
    /// Unique insights from a single lens that others missed
    pub surprising: Vec<SynthesisFinding>,
    /// Top 3 findings most directly applicable to the changes
    pub most_relevant: Vec<SynthesisFinding>,
}

/// A finding identified during cross-lens synthesis.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SynthesisFinding {
    /// Which lenses contributed to this finding
    pub lenses: Vec<ReviewerLens>,
    /// Summary of the cross-lens insight
    pub summary: String,
    /// Suggested action
    pub action: String,
}

/// Aggregated review result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AggregatedResult {
    /// Individual lens reviews
    pub per_lens: Vec<LensReview>,
    /// Deterministically computed overall signal
    pub overall_signal: Signal,
    /// Decision outcome derived from overall signal
    pub decision: Decision,
    /// Cross-lens synthesis (from Phase 5)
    pub cross_lens_synthesis: Option<CrossLensSynthesis>,
    /// Review metadata
    pub metadata: ReviewMetadata,
}

/// Decision outcome per the policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    Proceed,
    ProceedWithMitigation,
    BlockOrEscalate,
}

impl Decision {
    /// Human-readable display string.
    pub fn display(&self) -> &'static str {
        match self {
            Self::Proceed => "Proceed",
            Self::ProceedWithMitigation => "Proceed With Mitigation",
            Self::BlockOrEscalate => "Block or Escalate",
        }
    }

    /// Derive the decision from the overall signal.
    pub fn from_signal(signal: Signal) -> Self {
        match signal {
            Signal::Green | Signal::Gray => Self::Proceed,
            Signal::Yellow => Self::ProceedWithMitigation,
            Signal::Red => Self::BlockOrEscalate,
        }
    }
}

/// Metadata recorded with every review execution.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReviewMetadata {
    pub timestamp_utc: String,
    pub head_sha: String,
    pub base_branch: String,
    pub model: String,
    pub runner: String,
}

/// Context gathered from the git diff.
#[derive(Debug, Clone)]
pub struct DiffContext {
    pub base_branch: String,
    pub head_sha: String,
    pub diff_text: String,
    pub diff_stat: String,
    pub commit_messages: String,
    pub changed_files: Vec<String>,
}

/// Context gathered from ADR-managed content.
#[derive(Debug, Clone)]
pub struct AdrContext {
    pub policies: String,
    pub adr_sections: Vec<crate::generation::markers::AdrSection>,
    pub source_file: std::path::PathBuf,
}
