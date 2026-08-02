//! Simulation runtime: the runner (Scenario → Trace) and the tool
//! simulator. LLMs provide semantics (model outputs, tool responses,
//! state patches); code does the bookkeeping.

pub mod runner;
pub mod simulator;

pub use runner::{Runner, RunnerError};
pub use simulator::ToolSimulator;
