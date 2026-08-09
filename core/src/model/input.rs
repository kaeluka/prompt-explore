//! User-provided inputs: the prompts under test and the investigation.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One prompt under test: the system-prompt template, input variables,
/// tool surface, and (mandatory) design goals. The harness executes this
/// prompt inside scenario worlds and judges the resulting traces.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct PromptUnderTest {
    pub id: String,
    /// The system-prompt template. Placeholders use double braces:
    /// `{{variable_name}}`. Rules:
    /// - Name charset: `[A-Za-z0-9_]` (alphanumeric + underscore).
    /// - No spaces inside the braces — write `{{tier}}`, not `{{ tier }}`.
    /// - Each placeholder MUST have a matching key in the scenario's
    ///   `input_domain`; the simulator generates a concrete value for it
    ///   and substitutes it (strings inserted raw; other JSON values in
    ///   serialized form).
    /// - A template with no placeholders needs no `input_domain`.
    ///
    /// The opening user turn is separate — it comes from the scenario's
    /// `user_message`, not the template.
    pub template: String,
    /// This prompt's tool surface, exactly as the model sees it.
    /// Empty = no tool loop (but intent lives in `design_goals`, not here).
    pub tools: Vec<ToolSchema>,
    /// MANDATORY. The author's stated intent for the prompt — the
    /// yardstick it's supposed to uphold, and itself an optimization
    /// target. Advisory in the current
    /// verdict: the judge's criterion is the `question` alone; design
    /// goals are not automatically enforced during a run.
    pub design_goals: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ToolSchema {
    pub name: String,
    pub description: String,
    /// JSON Schema for the tool's parameters.
    pub parameters: Value,
    pub side_effect: SideEffect,
    /// Realism hints for the simulator LLM. These are anchors/examples,
    /// NOT pinned outputs — the simulator renders its own concrete
    /// responses from the narrative (see the API description's DESIGN
    /// INTENT: scripted/pinned tool responses are a deliberate non-goal).
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
