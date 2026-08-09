//! Run output: witnesses, attribution, and (unverified) proposals.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

use super::simulation::Trace;

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct RunResult {
    pub status: RunStatus,
    /// Scenarios attempted (= completed, in the response's `attempts`, +
    /// failed, in `failures`).
    pub scenarios_tried: u32,
    /// Per-scenario provenance labels (e.g. "caller-provided scenario
    /// 'id'"), surfaced so negative results show what was tried.
    pub strategies_tried: Vec<String>,
    pub witness: Option<Witness>,
    /// Advisory design-goal violations found across completed traces —
    /// best-effort: skipped when `design_goals` is empty or the goal
    /// judge errors. These do NOT affect the witness verdict (the
    /// question is the sole criterion); they are surfaced for the
    /// operator to read.
    pub incidental_findings: Vec<String>,
    /// Generated ONLY when a witness is found (fixes for the witnessed
    /// behavior). Empty on negative results — the proposer does not run
    /// without a witness. Always unverified; the user owns everything
    /// after the run.
    pub proposals: Vec<Proposal>,
    /// Scenarios that errored instead of producing a judged trace (PUT
    /// execution, tool simulation, or judge failure). When non-empty,
    /// `attempts` may be shorter than `scenarios_tried`; when ALL
    /// scenarios failed, `status` is `error`.
    #[serde(default)]
    pub failures: Vec<ScenarioFailure>,
    /// The world state at the end: the witness trace's when one was
    /// found, otherwise the last completed attempt's. Informational.
    #[serde(default)]
    pub final_state: Option<HashMap<String, Value>>,
}

/// A scenario that errored during a run.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ScenarioFailure {
    pub scenario_id: String,
    /// Where it failed: `"runner"` (PUT execution or tool simulation)
    /// or `"judge"`.
    pub stage: String,
    /// The error message.
    pub error: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    /// A trace where the questioned behavior occurred was found.
    WitnessFound,
    /// All scenarios completed a judged trace; none matched.
    NoWitnessWithinBudget,
    /// No witness, and the run was partial: some scenarios completed a
    /// judged trace (see `attempts`) and some errored (see `failures`).
    /// A no-witness read here is weaker than a complete one — part of
    /// the experiment never produced a trace.
    Partial,
    /// Every scenario errored; nothing was judged.
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct Witness {
    /// The matching trace. Currently always length 1 (existential mode
    /// only); differential/divergence questioning is not implemented.
    pub traces: Vec<Trace>,
    pub attribution: Attribution,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct Attribution {
    /// Verbatim quoted substrings of the PUT template implicated in the
    /// behavior. Currently always empty: with caller-provided scenarios
    /// there is no hypothesis to attribute instruction spans from.
    pub instruction_spans: Vec<String>,
    /// Free-text attribution note (e.g. the scenario id that produced
    /// the witness).
    pub evidence: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct Proposal {
    pub kind: ProposalKind,
    pub content: String,
    /// Instruction spans this proposal addresses.
    pub addresses: Vec<String>,
    /// Must state explicitly that the proposal is unverified.
    pub confidence_note: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProposalKind {
    Reword,
    Split,
    Merge,
    DataTransform,
    GoalRevision,
}

/// Result of checking a trace against a prompt's design goals.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct GoalFinding {
    pub goal: String,
    pub violated: bool,
    pub rationale: String,
    pub step_indices: Vec<usize>,
}

/// Result of comparing two traces for material divergence.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct DivergenceVerdict {
    pub divergent: bool,
    /// What specifically differs between the two traces.
    pub differing_aspect: Option<String>,
    pub rationale: String,
}
