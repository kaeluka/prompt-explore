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
    /// The system-prompt template. `{{var}}` placeholders are substituted
    /// from the scenario's `resolved_inputs`. The opening user turn is
    /// separate — it comes from the scenario's `user_message`, not the
    /// template.
    pub template: String,
    /// Documents the template's expected `{{variables}}` and how to
    /// generate values. With scenarios authored externally, this is
    /// metadata for authors; concrete values come from each scenario's
    /// `resolved_inputs`, which the runner substitutes into the template.
    pub input_vars: HashMap<String, VarSpec>,
    /// This prompt's tool surface, exactly as the model sees it.
    /// Empty = no tool loop (but intent lives in `design_goals`, not here).
    pub tools: Vec<ToolSchema>,
    /// MANDATORY. The author's stated intent for the prompt — the
    /// yardstick it's supposed to uphold, and itself an optimization
    /// target (`GoalRevision` proposals). Advisory in the current
    /// verdict: the judge's criterion is the `question` alone; design
    /// goals are not automatically enforced during a run.
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
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct Budget {
    /// Max steps per trace. A STEP is one tool call OR one final
    /// completion (the turn with no tool call that ends the trace). A
    /// completion that requests several tool calls counts as several
    /// steps. The main cost dial for tool-loop PUTs.
    pub max_steps_per_trace: u32,
    /// Optional per-trace token cap (input+output, summed across turns).
    pub max_tokens: Option<u64>,
}
