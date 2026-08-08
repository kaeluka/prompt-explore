//! Run output: witnesses, attribution, and (unverified) proposals.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

use super::simulation::Trace;

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct RunResult {
    pub status: RunStatus,
    /// How many scenarios completed a trace.
    pub scenarios_tried: u32,
    /// Per-scenario provenance labels (e.g. "caller-provided scenario
    /// 'id'"), surfaced so negative results show what was tried.
    pub strategies_tried: Vec<String>,
    pub witness: Option<Witness>,
    /// Reserved for goal violations found incidentally; currently always
    /// empty (goal checking is not wired into the run path).
    pub incidental_findings: Vec<String>,
    /// May be non-empty even on negative results (defensive hardening).
    /// Always unverified; the user owns everything after the run.
    pub proposals: Vec<Proposal>,
    /// The world state at the end: the witness trace's when one was
    /// found, otherwise the last completed attempt's. Informational.
    #[serde(default)]
    pub final_state: Option<HashMap<String, Value>>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    WitnessFound,
    NoWitnessWithinBudget,
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
