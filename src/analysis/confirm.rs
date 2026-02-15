/// The user's decision after reviewing the detected project analysis.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmAction {
    /// Accept all detected projects and proceed with sync.
    Accept,
    /// Reject the analysis and abort.
    Reject,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn confirm_action_clone() {
        let action = ConfirmAction::Accept;
        let cloned = action.clone();
        assert_eq!(action, cloned);
    }

    #[test]
    fn confirm_action_copy() {
        let action = ConfirmAction::Reject;
        let copied = action;
        // Both are still usable after copy (no move)
        assert_eq!(action, copied);
    }

    #[test]
    fn confirm_action_eq() {
        assert_eq!(ConfirmAction::Accept, ConfirmAction::Accept);
        assert_eq!(ConfirmAction::Reject, ConfirmAction::Reject);
        assert_ne!(ConfirmAction::Accept, ConfirmAction::Reject);
    }

    #[test]
    fn confirm_action_debug() {
        assert_eq!(format!("{:?}", ConfirmAction::Accept), "Accept");
        assert_eq!(format!("{:?}", ConfirmAction::Reject), "Reject");
    }
}
