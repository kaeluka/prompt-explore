//! Generation layer: proposal generation and application. (Scenario
//! generation was removed — scenarios are authored outside the harness;
//! see AGENTS.md's scenario-authoring guidance.)

pub mod apply;
pub mod propose;
pub mod search;

pub use apply::{AppliedPut, DiffPart, ProposalApplier};
pub use propose::ProposalGenerator;
pub use search::{Attempt, InvestigateOutcome, Investigator, LlmRole};
