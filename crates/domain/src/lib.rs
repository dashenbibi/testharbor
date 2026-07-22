//! TestHarbor domain: pure workflow, guards, and value objects.
//!
//! No I/O. Application and adapters depend on this crate; this crate depends on nothing else
//! in the workspace.

pub mod actions;
pub mod decisions;
pub mod guards;
pub mod hash;
pub mod ids;
pub mod intent;
pub mod run;
pub mod state;
pub mod transition;

pub use actions::{UserAction, UserActionDescriptor};
pub use decisions::*;
pub use guards::*;
pub use hash::*;
pub use ids::*;
pub use intent::*;
pub use run::{RunPointers, RunSummary};
pub use state::*;
pub use transition::{try_resume, try_transition, Transition, TransitionError, WorkflowEvent};
