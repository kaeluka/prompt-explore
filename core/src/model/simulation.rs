//! Simulation: scenarios (the reproducible seed) and traces
//! (the executed trajectory).

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

/// A test case: a world specification plus a protagonist. The harness
/// runs the prompt under test inside this world — the simulator LLM
/// renders tool responses from the `narrative` — and judges whether the
/// questioned behavior occurred in the resulting trace.
///
/// Scenarios are authored OUTSIDE the harness (by the operator's agent);
/// this API never generates them. Authoring guidance: the narrative
/// should pin (1) an inventory of what exists, covering every query type
/// the PUT's tools allow, (2) facts, including negative facts (what does
/// NOT exist or happen), (3) completeness assertions ("these are ALL the
/// entry points"), and (4) rendering rules (refuse queries outside the
/// inventory; filler introduces no new facts).
///
/// Everything needed to (stochastically) reproduce a trajectory lives
/// here; everything else in a trace is derived.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct Scenario {
    /// Free-form label, echoed back in reports.
    pub id: String,
    /// Provenance label: what this scenario was authored to test.
    /// Informational only.
    pub hypothesis_id: String,
    /// Provenance: which prompt this scenario was authored for. NOT
    /// enforced — a scenario may be run against any PUT.
    pub put_id: String,
    /// Concrete values for the PUT template's {{variables}}.
    pub resolved_inputs: HashMap<String, Value>,
    /// The opening message from the user/protagonist. For a tool-less
    /// PUT this is the entire work input.
    pub user_message: Option<String>,
    /// Mutable world facts, updated by write-tool patches during the
    /// trace. Static truth belongs in the narrative, not here.
    pub world_state: HashMap<String, Value>,
    /// Persona/stance guidance for a simulated user, if the scenario
    /// involves one.
    pub simulator_notes: String,
    /// The world specification — ground truth the simulator renders
    /// tool responses from, and the judge checks claims against.
    /// Natural language; see the struct docs for the four parts it
    /// should pin.
    #[serde(default)]
    pub narrative: String,
    /// Operator-stated environment state, verbatim from the
    /// investigation's `initial_state`. Kept separate from
    /// `simulator_notes` so judge, simulator, and UI see the operator's
    /// words, not a paraphrase.
    #[serde(default)]
    pub stated_state: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct TraceStep {
    pub model_output: String,
    pub tool_call: Option<ToolCall>,
    pub tool_response: Option<Value>,
    /// Present on write-tool steps.
    pub world_state_after: Option<HashMap<String, Value>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ToolCall {
    pub name: String,
    pub args: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct Trace {
    pub scenario_id: String,
    pub steps: Vec<TraceStep>,
    /// The world state at the end of the run (after all applied
    /// patches). Empty if no write tool ever ran.
    #[serde(default)]
    pub final_world_state: HashMap<String, Value>,
    /// Set by the judge after the runner produces the trace.
    pub verdict: Option<Verdict>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct Verdict {
    /// Does this trace satisfy the predicate (∧ design_goals)?
    pub matched: bool,
    /// The judge's confidence in its own verdict (self-reported).
    pub confidence: Option<f32>,
    pub rationale: String,
    /// Where in the trace the match happened.
    pub matched_step_indices: Vec<usize>,
}
