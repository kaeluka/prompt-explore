//! prompt-explore: property-based testing for agent behavior.
//!
//! The user supplies scenarios (author-supplied world narratives) and a
//! prompt under test. Each investigation call runs one scenario inside its
//! simulated world and surfaces complete evidence — world, input domain,
//! resolved inputs, and the full trace of model turns. Callers run a corpus
//! by making one call per scenario. The caller is the judge: they read the
//! traces and decide what (if anything) to fix. The run's `reason` is advisory
//! framing, not an oracle. There is no in-harness verdict.

pub mod assessment;
pub mod frontier;
pub mod generate;
pub mod llm;
pub mod model;
pub mod simulate;
