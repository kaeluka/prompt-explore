//! Generation layer: the investigation orchestrator (`search`). (Scenario
//! generation was removed — scenarios are authored outside the harness.
//! Proposal generation and application were also removed: the witness and
//! trace are the deliverable; the fix is the caller's job. See AGENTS.md.)

pub mod search;

pub use search::{Attempt, InvestigateOutcome, Investigator, LlmRole};
