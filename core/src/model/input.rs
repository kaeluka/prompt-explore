//! User-provided inputs: the prompts under test and the investigation.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct PromptsUnderTest {
    pub prompts: Vec<PromptUnderTest>,
    /// NL description of how the PUTs connect (pipeline, usually a DAG).
    /// Used for hypothesis generation and cross-prompt proposals;
    /// NOT executed — a run targets one PUT at a time.
    pub topology: String,
    /// Pipeline-level constraints, e.g. "never promise refunds above $500".
    pub design_goals: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct PromptUnderTest {
    pub id: String,
    pub template: String,
    pub input_vars: HashMap<String, VarSpec>,
    /// This prompt's tool surface, exactly as the model sees it.
    /// Empty = no tool loop (but intent lives in `design_goals`, not here).
    pub tools: Vec<ToolSchema>,
    /// MANDATORY. The yardstick for judging behavior; also itself an
    /// optimization target (flagged via `ProposalKind::GoalRevision`).
    pub design_goals: String,
}

/// Extensible per-variable data-generation spec.
///
/// New variants must be additive; the serialized form stays
/// self-describing via the `kind` tag.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum VarSpec {
    Constant { value: Value },
    NlDescription { description: String },
    Examples { examples: Vec<Value> },
    // future: Schema, Distribution, TraceSample, ...
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ToolSchema {
    pub name: String,
    pub description: String,
    /// JSON Schema for the tool's parameters.
    pub parameters: Value,
    pub side_effect: SideEffect,
    /// Optional realism anchors for the simulator LLM.
    #[serde(default)]
    pub example_responses: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SideEffect {
    Read,
    Write,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct Investigation {
    /// The mandatory question, e.g. "are there inputs that cause
    /// destructive tool calls?" or "why does this sometimes cancel,
    /// sometimes ask to confirm?"
    pub question: String,
    /// PUT id — a run executes exactly one PUT.
    pub target_put: String,
    pub budget: Budget,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct Budget {
    pub max_scenarios: u32,
    pub max_steps_per_trace: u32,
    pub max_tokens: Option<u64>,
}
