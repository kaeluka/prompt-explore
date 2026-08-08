//! Simulation: scenarios (the reproducible seed) and traces
//! (the executed trajectory).

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

/// Everything needed to (stochastically) reproduce a trajectory.
/// Everything else in a trace is derived.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Scenario {
    pub id: String,
    pub hypothesis_id: String,
    pub put_id: String,
    /// Concrete template vars (constants copied, descriptions resolved).
    pub resolved_inputs: HashMap<String, Value>,
    pub user_message: Option<String>,
    pub world_state: HashMap<String, Value>,
    /// Persona/stance guidance for the simulator LLM.
    pub simulator_notes: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceStep {
    pub model_output: String,
    pub tool_call: Option<ToolCall>,
    pub tool_response: Option<Value>,
    /// Present on write-tool steps.
    pub world_state_after: Option<HashMap<String, Value>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub name: String,
    pub args: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Trace {
    pub scenario_id: String,
    pub steps: Vec<TraceStep>,
    /// Set by the judge after the runner produces the trace.
    pub verdict: Option<Verdict>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Verdict {
    /// Does this trace satisfy the predicate (∧ design_goals)?
    pub matched: bool,
    /// The judge's confidence in its own verdict (self-reported).
    pub confidence: Option<f32>,
    pub rationale: String,
    /// Where in the trace the match happened.
    pub matched_step_indices: Vec<usize>,
}
