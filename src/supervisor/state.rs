use crate::supervisor::task::TaskStatus as SupervisorState;

pub fn transition_allowed(from: SupervisorState, to: SupervisorState) -> bool {
    use SupervisorState::*;
    matches!(
        (from, to),
        (Intake, Classify)
            | (Classify, Route)
            | (Route, Clarify)
            | (Route, Plan)
            | (Route, Execute)
            | (Clarify, Plan)
            | (Clarify, Execute)
            | (Clarify, Cancelled)
            | (Plan, PrepareWorkspace)
            | (Plan, Execute)
            | (Plan, Cancelled)
            | (PrepareWorkspace, Execute)
            | (PrepareWorkspace, Cancelled)
            | (Execute, Plan)
            | (Execute, Review)
            | (Execute, Verify)
            | (Execute, Failed)
            | (Execute, Paused)
            | (Execute, Cancelled)
            | (Review, Verify)
            | (Review, Execute)
            | (Review, Cancelled)
            | (Verify, Report)
            | (Verify, Execute)
            | (Verify, Failed)
            | (Report, Archive)
            | (Archive, Done)
            | (Paused, Execute)
            | (Paused, Plan)
            | (Paused, Cancelled)
            | (Route, Cancelled)
            | (Route, Paused)
            | (Plan, Paused)
            | (PrepareWorkspace, Paused)
            | (Intake, Cancelled)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_transitions_succeed_and_invalid_fail() {
        use SupervisorState::*;
        assert!(transition_allowed(Intake, Classify));
        assert!(transition_allowed(Classify, Route));
        assert!(transition_allowed(Route, Clarify));
        assert!(transition_allowed(Verify, Report));
        assert!(transition_allowed(Execute, Failed));
        assert!(!transition_allowed(Intake, Done));
        assert!(!transition_allowed(Done, Execute));
        // Terminal states must not transition to Cancelled
        assert!(!transition_allowed(Done, Cancelled));
        assert!(!transition_allowed(Failed, Cancelled));
    }
}
