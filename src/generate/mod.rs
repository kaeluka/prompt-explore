//! Generation layer: turns a bare behavioral question into scenarios,
//! via hypothesis generation and scenario building. The search loop
//! (`Investigator`) orchestrates hypothesize → build → run → judge.

pub mod hypothesize;
pub mod scenario;
pub mod search;

pub use hypothesize::Hypothesizer;
pub use scenario::ScenarioBuilder;
pub use search::{InvestigateOutcome, Investigator, LlmRole};
