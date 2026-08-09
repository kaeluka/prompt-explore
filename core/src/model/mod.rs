//! The pure model layer.
//!
//! Data types for one investigation iteration:
//! `input` (PUT + investigation) → `simulation` (scenarios + traces) →
//! `output` (status, attempts, failures). There is no predicate layer
//! and no verdict: the harness runs scenarios and surfaces traces; the
//! caller is the judge.
//!
//! This layer has no runtime dependencies: no LLM clients, no I/O,
//! no async. Everything here is plain serializable data.

pub mod input;
pub mod output;
pub mod simulation;

pub use input::*;
pub use output::*;
pub use simulation::*;
