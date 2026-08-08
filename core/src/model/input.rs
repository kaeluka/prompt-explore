//! User-provided inputs: the prompts under test and the investigation.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

/// The prompts under test (PsUT): a set of agent prompts, how they
/// connect, and optional pipeline-level design goals.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct PromptsUnderTest {
    pub prompts: Vec<PromptUnderTest>,
    /// NL description of how the prompts connect (pipeline, usually a DAG).
    /// Used for hypothesis generation and cross-prompt proposals;
    /// NOT executed — a run targets one prompt at a time.
    pub topology: String,
    /// Pipeline-level constraints, e.g. "never promise refunds above $500".
    pub design_goals: Option<String>,
}

/// One prompt under test: template, input variables, tool surface,
/// and (mandatory) design goals.
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
    /// Id of the prompt under test — a run executes exactly one prompt.
    pub target_put: String,
    pub budget: Budget,
    /// Optional user-specified starting environment state, in natural
    /// language (e.g. "cancel_order is broken and returns E_CONN; order
    /// 123 is already shipped"). It is NOT compiled or enforced: it
    /// flows into scenario building, the simulator notes, and the
    /// judge's scenario context as-is. Free-text — pasting a previous
    /// run's returned `final_state` JSON works fine.
    #[serde(default)]
    pub initial_state: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct Budget {
    pub max_scenarios: u32,
    pub max_steps_per_trace: u32,
    pub max_tokens: Option<u64>,
}
