use super::types::{AggregatedResult, Decision, LensReview, ReviewerLens, Signal};

impl Signal {
    /// Maps a signal to its corresponding decision per the Change Risk Signaling Policy.
    pub fn to_decision(&self) -> Decision {
        match self {
            Signal::Red => Decision::BlockOrEscalate,
            Signal::Yellow => Decision::ProceedWithMitigation,
            Signal::Green | Signal::Gray => Decision::Proceed,
        }
    }
}

/// Deterministically aggregates lens reviews into an overall signal and decision.
///
/// Rules (from Section 2.3 of the Change Risk Signaling Policy):
/// 1. Filter out Gray signals
/// 2. If no non-gray reviews → Green / Proceed
/// 3. If Security or SRE is Red → Red / BlockOrEscalate
/// 4. If any lens is Red → Red / BlockOrEscalate
/// 5. If 3+ Yellow signals → Red / BlockOrEscalate (escalation)
/// 6. If any Yellow → Yellow / ProceedWithMitigation
/// 7. Otherwise → Green / Proceed
pub fn aggregate_signals(reviews: &[LensReview]) -> AggregatedResult {
    let non_gray: Vec<&LensReview> = reviews
        .iter()
        .filter(|r| r.signal != Signal::Gray)
        .collect();

    let (overall_signal, decision) = if non_gray.is_empty() {
        (Signal::Green, Decision::Proceed)
    } else if reviews.iter().any(|r| {
        r.signal == Signal::Red
            && matches!(
                r.lens,
                ReviewerLens::SecurityEngineer | ReviewerLens::SreEngineer
            )
    }) {
        (Signal::Red, Decision::BlockOrEscalate)
    } else if non_gray.iter().any(|r| r.signal == Signal::Red) {
        (Signal::Red, Decision::BlockOrEscalate)
    } else {
        let yellow_count = non_gray.iter().filter(|r| r.signal == Signal::Yellow).count();
        if yellow_count >= 3 {
            (Signal::Red, Decision::BlockOrEscalate)
        } else if yellow_count > 0 {
            (Signal::Yellow, Decision::ProceedWithMitigation)
        } else {
            (Signal::Green, Decision::Proceed)
        }
    };

    AggregatedResult {
        per_lens: reviews.to_vec(),
        overall_signal,
        decision,
        cross_lens_synthesis: None,
        metadata: Default::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::commands::cr::types::{Decision, LensReview, ReviewerLens, Signal};

    fn make_review(lens: ReviewerLens, signal: Signal) -> LensReview {
        LensReview {
            lens,
            signal,
            rationale: String::new(),
            findings: vec![],
        }
    }

    #[test]
    fn all_green() {
        let reviews = vec![
            make_review(ReviewerLens::PrincipalEngineer, Signal::Green),
            make_review(ReviewerLens::ProgramManager, Signal::Green),
            make_review(ReviewerLens::SecurityEngineer, Signal::Green),
        ];
        let result = aggregate_signals(&reviews);
        assert_eq!(result.overall_signal, Signal::Green);
        assert_eq!(result.decision, Decision::Proceed);
    }

    #[test]
    fn all_gray_treated_as_green() {
        let reviews = vec![
            make_review(ReviewerLens::PrincipalEngineer, Signal::Gray),
            make_review(ReviewerLens::ProgramManager, Signal::Gray),
        ];
        let result = aggregate_signals(&reviews);
        assert_eq!(result.overall_signal, Signal::Green);
        assert_eq!(result.decision, Decision::Proceed);
    }

    #[test]
    fn empty_reviews() {
        let result = aggregate_signals(&[]);
        assert_eq!(result.overall_signal, Signal::Green);
        assert_eq!(result.decision, Decision::Proceed);
    }

    #[test]
    fn security_red_auto_blocks() {
        let reviews = vec![
            make_review(ReviewerLens::PrincipalEngineer, Signal::Green),
            make_review(ReviewerLens::SecurityEngineer, Signal::Red),
            make_review(ReviewerLens::ProgramManager, Signal::Green),
        ];
        let result = aggregate_signals(&reviews);
        assert_eq!(result.overall_signal, Signal::Red);
        assert_eq!(result.decision, Decision::BlockOrEscalate);
    }

    #[test]
    fn sre_red_auto_blocks() {
        let reviews = vec![
            make_review(ReviewerLens::PrincipalEngineer, Signal::Green),
            make_review(ReviewerLens::SreEngineer, Signal::Red),
        ];
        let result = aggregate_signals(&reviews);
        assert_eq!(result.overall_signal, Signal::Red);
        assert_eq!(result.decision, Decision::BlockOrEscalate);
    }

    #[test]
    fn any_red_blocks() {
        let reviews = vec![
            make_review(ReviewerLens::PrincipalEngineer, Signal::Red),
            make_review(ReviewerLens::ProgramManager, Signal::Green),
        ];
        let result = aggregate_signals(&reviews);
        assert_eq!(result.overall_signal, Signal::Red);
        assert_eq!(result.decision, Decision::BlockOrEscalate);
    }

    #[test]
    fn three_yellows_escalate_to_red() {
        let reviews = vec![
            make_review(ReviewerLens::PrincipalEngineer, Signal::Yellow),
            make_review(ReviewerLens::ProgramManager, Signal::Yellow),
            make_review(ReviewerLens::ProductManager, Signal::Yellow),
            make_review(ReviewerLens::SecurityEngineer, Signal::Green),
        ];
        let result = aggregate_signals(&reviews);
        assert_eq!(result.overall_signal, Signal::Red);
        assert_eq!(result.decision, Decision::BlockOrEscalate);
    }

    #[test]
    fn two_yellows_stay_yellow() {
        let reviews = vec![
            make_review(ReviewerLens::PrincipalEngineer, Signal::Yellow),
            make_review(ReviewerLens::ProgramManager, Signal::Yellow),
            make_review(ReviewerLens::SecurityEngineer, Signal::Green),
        ];
        let result = aggregate_signals(&reviews);
        assert_eq!(result.overall_signal, Signal::Yellow);
        assert_eq!(result.decision, Decision::ProceedWithMitigation);
    }

    #[test]
    fn single_yellow() {
        let reviews = vec![
            make_review(ReviewerLens::PrincipalEngineer, Signal::Yellow),
            make_review(ReviewerLens::ProgramManager, Signal::Green),
        ];
        let result = aggregate_signals(&reviews);
        assert_eq!(result.overall_signal, Signal::Yellow);
        assert_eq!(result.decision, Decision::ProceedWithMitigation);
    }

    #[test]
    fn gray_reviews_excluded_from_yellow_count() {
        // 2 Yellow + 1 Gray should NOT escalate to Red
        let reviews = vec![
            make_review(ReviewerLens::PrincipalEngineer, Signal::Yellow),
            make_review(ReviewerLens::ProgramManager, Signal::Yellow),
            make_review(ReviewerLens::ProductManager, Signal::Gray),
        ];
        let result = aggregate_signals(&reviews);
        assert_eq!(result.overall_signal, Signal::Yellow);
        assert_eq!(result.decision, Decision::ProceedWithMitigation);
    }

    #[test]
    fn signal_to_decision_mapping() {
        assert_eq!(Signal::Red.to_decision(), Decision::BlockOrEscalate);
        assert_eq!(Signal::Yellow.to_decision(), Decision::ProceedWithMitigation);
        assert_eq!(Signal::Green.to_decision(), Decision::Proceed);
        assert_eq!(Signal::Gray.to_decision(), Decision::Proceed);
    }

    #[test]
    fn synthesis_is_none_from_aggregation() {
        let reviews = vec![make_review(ReviewerLens::PrincipalEngineer, Signal::Green)];
        let result = aggregate_signals(&reviews);
        assert!(result.cross_lens_synthesis.is_none());
    }

    #[test]
    fn per_lens_preserved() {
        let reviews = vec![
            make_review(ReviewerLens::PrincipalEngineer, Signal::Green),
            make_review(ReviewerLens::SecurityEngineer, Signal::Yellow),
        ];
        let result = aggregate_signals(&reviews);
        assert_eq!(result.per_lens.len(), 2);
        assert_eq!(result.per_lens[0].lens, ReviewerLens::PrincipalEngineer);
        assert_eq!(result.per_lens[1].lens, ReviewerLens::SecurityEngineer);
    }
}
