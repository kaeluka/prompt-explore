//! User-provided inputs: the prompts under test and the investigation.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One prompt under test: the system-prompt template, input variables,
/// tool surface, and design goals. The harness executes this prompt
/// inside scenario worlds and surfaces the resulting traces for the
/// caller to judge.
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
    /// The author's stated intent for the prompt — documentation the
    /// caller reads when judging traces. No longer judged in-harness
    /// (the judge was removed): it is surfaced with the result as
    /// framing, not enforced. Still an optimization target for the
    /// caller, who holds the intent.
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

/// An investigation: run the given scenarios against the PUT and
/// surface the resulting traces. Nothing is judged in-harness — the
/// caller reads the traces and judges.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct Investigation {
    /// Advisory framing for the CALLER — what the caller is worried
    /// about. Surfaced with the result to guide reading the traces;
    /// never used as an oracle. The harness runs scenarios and surfaces
    /// evidence; the caller is the judge. Optional — omit it when you
    /// just want to observe behavior with no particular axe to grind.
    ///
    /// e.g. "are there inputs that cause destructive tool calls?" or
    /// "why does this sometimes cancel, sometimes ask to confirm?"
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub question: Option<String>,
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
