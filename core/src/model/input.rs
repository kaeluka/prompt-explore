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
    /// Variables are placeholders for things meant to VARY per scenario —
    /// the simulator LLM invents each concrete value from the domain
    /// description. Text under test does NOT belong in a placeholder:
    /// bake it into the template verbatim. Routing constant text through
    /// a placeholder hands it to the simulator to (re)generate — it may be
    /// paraphrased, or silently dropped from `resolved_inputs`, so the
    /// episode runs without the very text being tested. When the complete
    /// literal already is the intended value, the simulator tends to copy
    /// it — but that is a tendency, not a contract. Placeholders are for
    /// inputs the scenario should sample, not for the prompt itself.
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
    /// The PUT's actual tool contract, also used by the simulator. Describe
    /// argument semantics AND returned shape: for repository tools, define root
    /// aliases, literal vs regex search (and grammar), line numbering, errors,
    /// truncation and which files belong to the inventory. A vague 'pattern'
    /// lets LLM and Lua implementations disagree silently. Example: 'path . or
    /// empty means root; search is literal case-sensitive substring; return
    /// {matches:[{path,line,text}],truncated}; no match is an empty array'.
    /// These are caller-supplied semantics, not a built-in harness tool surface.
    pub description: String,
    /// JSON Schema for the tool's parameters.
    pub parameters: Value,
    pub side_effect: SideEffect,
    /// Realism hints for the simulator LLM. These are anchors/examples,
    /// NOT pinned outputs — the simulator renders its own concrete
    /// responses from the narrative (see the API description's DESIGN
    /// INTENT). In experimental hybrid mode it may generate executable Lua
    /// handlers; these examples remain hints, not forced return values.
    #[serde(default)]
    pub example_responses: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SideEffect {
    Read,
    Write,
}

/// An investigation's controls: run one supplied scenario against the PUT
/// and surface its resulting trace. Callers that want a corpus run invoke the
/// singular operation once per scenario. Nothing is judged in-harness — the
/// caller reads the trace and judges.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct Investigation {
    /// Free-form justification for the run — WHY it exists and what a
    /// reader should know when comparing it with earlier runs: what it
    /// aims to accomplish, what changed compared to previous runs (a
    /// prompt edit, new scenarios, a different model), anything that
    /// frames how to read the traces. There is no strict standard —
    /// write whatever makes the run intelligible later.
    ///
    /// Advisory only: surfaced with the result to guide reading the
    /// traces, NEVER used as an oracle. The harness runs a scenario and
    /// surfaces evidence; the caller is the judge. Optional — omit it
    /// when you just want to observe behavior with no particular
    /// framing.
    ///
    /// e.g. "baseline before adding the explicit-confirmation rule" or
    /// "re-run after softening the refusal instruction; compare with v3".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub budget: Budget,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct Budget {
    /// Max steps per trace. A STEP is one tool call OR one final
    /// completion (the turn with no tool call that ends the trace). A
    /// completion that requests several tool calls counts as several
    /// steps but is an atomic batch: every sibling call is simulated, so
    /// one accepted batch may cross this cap. No later PUT turn then runs.
    /// The main cost dial for tool-loop PUTs. Reserve room for the final
    /// completion. A trace recorded at the cap can lack a final answer; inspect
    /// execution.stop_reason and counters rather than equating done with success.
    pub max_steps_per_trace: u32,
    /// Optional PUT input+output token cap, summed across completions (repeated
    /// conversation history is counted on every completion). Simulator tokens
    /// are not part of this cap. See execution.put_tokens_used and stop_reason.
    pub max_tokens: Option<u64>,
}
