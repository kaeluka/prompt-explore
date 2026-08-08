//! User-provided inputs: the prompts under test and the investigation.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

/// One prompt under test: the system-prompt template, input variables,
/// tool surface, and (mandatory) design goals. The harness executes this
/// prompt inside scenario worlds and judges the resulting traces.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct PromptUnderTest {
    pub id: String,
    pub template: String,
    pub input_vars: HashMap<String, VarSpec>,
    /// This prompt's tool surface, exactly as the model sees it.
    /// Empty = no tool loop (but intent lives in `design_goals`, not here).
    pub tools: Vec<ToolSchema>,
    /// MANDATORY. The yardstick for judging behavior: the intent the
    /// prompt is supposed to uphold. Also itself an optimization target
    /// (flagged via `ProposalKind::GoalRevision`).
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

/// An investigation: run the given scenarios against the PUT and judge
/// every trace against the question.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct Investigation {
    /// The mandatory behavioral question, used VERBATIM as the judge's
    /// criterion — e.g. "are there inputs that cause destructive tool
    /// calls?" or "why does this sometimes cancel, sometimes ask to
    /// confirm?" A witness is a trace where the judge finds the
    /// questioned behavior actually occurred.
    pub question: String,
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
    pub max_steps_per_trace: u32,
    pub max_tokens: Option<u64>,
}
