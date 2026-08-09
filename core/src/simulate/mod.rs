//! Simulation runtime: the runner (Scenario → Trace) and the tool
//! simulator. LLMs provide semantics (model outputs, tool responses,
//! state patches); code does the bookkeeping. The harness surfaces the
//! traces; the caller is the judge.

pub mod runner;
pub mod simulator;
pub mod transcript;

pub use runner::{Runner, RunnerError};
pub use simulator::ToolSimulator;
pub use transcript::render_transcript;
