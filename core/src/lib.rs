//! prompt-explore: property-based testing for agent behavior.
//!
//! The user supplies scenarios (author-supplied world narratives) and a
//! prompt under test. The tool runs every scenario inside the simulated
//! world and surfaces complete evidence — world, input domain, resolved
//! inputs, and the full trace of steps. The caller is the judge: they
//! read the traces and decide what (if anything) to fix. The question is
//! advisory framing, not an oracle. There is no in-harness verdict.

pub mod generate;
pub mod llm;
pub mod model;
pub mod simulate;
