//! Run output: witnesses, attribution, and (unverified) proposals.

use serde::{Deserialize, Serialize};

use super::simulation::Trace;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunResult {
    pub status: RunStatus,
    pub scenarios_tried: u32,
    /// Hypothesis summaries — shown on negative results.
    pub strategies_tried: Vec<String>,
    pub witness: Option<Witness>,
    /// Goal violations found incidentally during the search.
    pub incidental_findings: Vec<String>,
    /// May be non-empty even on negative results (defensive hardening).
    /// Always unverified; the user owns everything after the run.
    pub proposals: Vec<Proposal>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    WitnessFound,
    NoWitnessWithinBudget,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Witness {
    /// 1 trace for existential questions, 2 for divergence.
    pub traces: Vec<Trace>,
    pub attribution: Attribution,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Attribution {
    pub instruction_spans: Vec<String>,
    /// e.g. ablation summary
    pub evidence: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Proposal {
    pub kind: ProposalKind,
    pub content: String,
    /// Instruction spans this proposal addresses.
    pub addresses: Vec<String>,
    /// Must state explicitly that the proposal is unverified.
    pub confidence_note: String,
    /// Structured edits, when the proposal is syntactically appliable.
    /// Each `find` must be verbatim text occurring exactly once in the
    /// target. Empty for purely advisory proposals.
    #[serde(default)]
    pub edits: Vec<TextEdit>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextEdit {
    pub target: EditTarget,
    /// Verbatim text to find (must occur exactly once).
    pub find: String,
    pub replace: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EditTarget {
    Template,
    DesignGoals,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProposalKind {
    Reword,
    Split,
    Merge,
    DataTransform,
    GoalRevision,
}

/// Result of checking a trace against a PUT's design goals.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoalFinding {
    pub goal: String,
    pub violated: bool,
    pub rationale: String,
    pub step_indices: Vec<usize>,
}

/// Result of comparing two traces for material divergence.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DivergenceVerdict {
    pub divergent: bool,
    /// What specifically differs between the two traces.
    pub differing_aspect: Option<String>,
    pub rationale: String,
}
