//! Generation layer: turns a bare behavioral question into scenarios,
//! via hypothesis generation and scenario building. The search loop
//! (`Investigator`) orchestrates hypothesize → build → run → judge.

pub mod apply;
pub mod hypothesize;
pub mod propose;
pub mod scenario;
pub mod search;

pub use apply::{AppliedPut, DiffPart, ProposalApplier};
pub use hypothesize::Hypothesizer;
pub use propose::ProposalGenerator;
pub use scenario::ScenarioBuilder;
pub use search::{Attempt, InvestigateOutcome, Investigator, LlmRole};
