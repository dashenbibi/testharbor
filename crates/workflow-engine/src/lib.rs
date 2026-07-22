//! Workflow engine facade. This crate is the single pure entry point used by
//! application transactions to evaluate state transitions.

pub use testharbor_domain::{
    try_resume, try_transition, Transition, TransitionError, WorkflowEvent, WorkflowState,
};

pub fn require_transition(
    current: WorkflowState,
    event: WorkflowEvent,
) -> Result<WorkflowState, TransitionError> {
    try_transition(current, event).map(|transition| transition.to)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unlisted_edge() {
        assert!(require_transition(WorkflowState::Created, WorkflowEvent::Apply).is_err());
    }
}
