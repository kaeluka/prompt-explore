//! Run output: status, attempts, and failures. The harness runs
//! scenarios and surfaces complete evidence (world, input domain,
//! resolved inputs, full steps); the CALLER is the judge — there is
//! no in-harness verdict, witness, or attribution.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

use super::simulation::Scenario;

/// The outcome of running a set of scenarios against a PUT. The
/// harness's job ends here: every scenario that completed has a trace
/// in `attempts`; every scenario that errored is in `failures`. There
/// is no verdict — the caller reads the traces and judges.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct RunResult {
    pub status: RunStatus,
    /// Scenarios attempted (= completed, in the response's `attempts`, +
    /// failed, in `failures`).
    pub scenarios_tried: u32,
    /// Scenarios that errored instead of producing a trace (PUT
    /// execution, input resolution, or tool simulation). When non-empty,
    /// `attempts` may be shorter than `scenarios_tried`; when ALL
    /// scenarios failed, `status` is `error`.
    #[serde(default)]
    pub failures: Vec<ScenarioFailure>,
    /// The world state at the end: the last completed attempt's final
    /// state. Informational.
    #[serde(default)]
    pub final_state: Option<HashMap<String, Value>>,
}

/// A scenario that errored during a run.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ScenarioFailure {
    pub scenario: Scenario,
    /// Where it failed: `"runner"` (PUT execution, input resolution, or
    /// tool simulation).
    pub stage: String,
    /// The error message.
    pub error: String,
}

/// Run-completion taxonomy. With the judge removed, "completion" is
/// purely about whether scenarios produced traces — not whether any
/// matched a question. The caller judges the traces.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    /// Every scenario produced a trace (see `attempts`).
    Completed,
    /// Some scenarios completed a trace (see `attempts`) and some
    /// errored (see `failures`). Part of the evidence is missing.
    Partial,
    /// Every scenario errored; no traces were produced.
    Error,
}
