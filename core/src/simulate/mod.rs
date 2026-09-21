//! Simulation runtime: the runner (Scenario → Trace) and the tool
//! simulator. LLMs provide semantics (model outputs, tool responses,
//! state patches); code does the bookkeeping. The harness surfaces the
//! traces; the caller is the judge.

pub mod engine;
pub mod lua;
pub mod runner;
pub mod simulator;
pub mod transcript;
pub mod workflow;
pub mod workspace;

pub use engine::{ScenarioRuntime, SimEngine, template_variables};
pub use runner::{
    DEFAULT_PUT_MAX_TOKENS, DEFAULT_PUT_TEMPERATURE, Runner, RunnerError, RunnerOptions,
};
pub use simulator::{
    DEFAULT_MAX_WORKSPACE_TURNS, DEFAULT_SIMULATOR_MAX_TOKENS, DEFAULT_SIMULATOR_REPAIR_ATTEMPTS,
    DEFAULT_SIMULATOR_TEMPERATURE, SimulatorOptions, ToolSimulator,
};
pub use transcript::render_transcript;
pub(crate) use workflow::run_workflow;
pub use workspace::{
    Workspace, WorkspaceError, WorkspaceToolLimits, unpack_zip, unpack_zip_with_limits,
};
