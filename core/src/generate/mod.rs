//! Generation layer: the investigation orchestrator (`search`). Scenarios
//! are authored outside the harness; the orchestrator runs every one
//! against the PUT and surfaces the traces. There is no judge and no
//! proposal generation — the traces are the deliverable and the caller
//! is the judge. See AGENTS.md.

pub mod search;

pub use search::{Attempt, InvestigateOutcome, Investigator, LlmRole};
