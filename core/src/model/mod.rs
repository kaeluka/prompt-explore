//! The pure model layer.
//!
//! Data types for one investigation iteration:
//! `input` (PsUT + investigation) → `predicate` (operationalized question
//! + hypotheses) → `simulation` (scenarios + traces) → `output`
//! (witness, verdicts, incidental findings).
//!
//! This layer has no runtime dependencies: no LLM clients, no I/O,
//! no async. Everything here is plain serializable data.

pub mod input;
pub mod output;
pub mod predicate;
pub mod simulation;

pub use input::*;
pub use output::*;
pub use predicate::*;
pub use simulation::*;
