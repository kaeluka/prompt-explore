//! The pure model layer.
//!
//! Data types for one investigation iteration:
//! `input` (PUT + investigation + one scenario) → `simulation` (trace) →
//! `output` (a run failure, when no trace was produced). There is no predicate
//! layer and no verdict: the harness runs one scenario and surfaces its trace;
//! the caller is the judge.
//!
//! This layer has no runtime dependencies: no LLM clients, no I/O,
//! no async. Everything here is plain serializable data.

pub mod input;
pub mod lua;
pub mod output;
pub mod scenario;
pub mod simulation;

pub use input::*;
pub use lua::LuaOptions;
pub use output::*;
pub use scenario::*;
pub use simulation::*;
