//! Generation layer: the investigation orchestrator (`search`). Scenarios
//! are authored outside the harness; each call runs one scenario and surfaces
//! its trace. There is no judge and no proposal generation — the trace is the
//! deliverable and the caller is the judge. See AGENTS.md.

pub mod search;

pub use search::{InvestigateOutcome, Investigator, LlmRole};
