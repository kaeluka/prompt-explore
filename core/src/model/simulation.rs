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
    /// Required; every scenario must pin one (see the struct docs for
    /// the four parts it should cover).
    pub narrative: String,
    /// Operator-required environment facts for THIS scenario (e.g.
    /// "cancel_order is broken and returns E_CONN"). Appended to the
    /// simulator's context so it respects them. Independent per scenario;
    /// there is no investigation-level environment-state field.
    #[serde(default)]
    pub stated_state: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct TraceStep {
    /// The model's text output for this turn (empty on non-first tool
    /// calls within one completion).
    pub model_output: String,
    /// The tool the model asked to call, if any. A completion that
    /// requests N tool calls becomes N steps.
    pub tool_call: Option<ToolCall>,
    /// The simulated tool response.
    pub tool_response: Option<Value>,
    /// Present on write-tool steps: world state after the patch applied.
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
    /// Whether the judge finds the questioned behavior (the
    /// investigation's `question`, used verbatim) ACTUALLY occurred in
    /// this trace. Design goals are NOT anded in — they're an advisory
    /// yardstick and a separate optimization target, not enforced here.
    pub matched: bool,
    /// The judge's confidence in its own verdict (self-reported).
    pub confidence: Option<f32>,
    pub rationale: String,
    /// Where in the trace the match happened.
    pub matched_step_indices: Vec<usize>,
}
