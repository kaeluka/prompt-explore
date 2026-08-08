//! Scenario building: hypothesis + PUT → one or more concrete scenarios.
//!
//! Each scenario's core is the **narrative**: a world specification
//! (inventory, facts incl. negatives, completeness assertions, rendering
//! rules) that is ground truth for the simulator, the judge, and the UI.
//! The user message, initial world_state, resolved inputs, and simulator
//! notes accompany it.

use std::sync::Arc;

use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::llm::{ChatRequest, LlmClient, LlmError, Message};
use crate::model::input::{PromptUnderTest, VarSpec};
use crate::model::predicate::Hypothesis;
use crate::model::simulation::Scenario;

pub struct ScenarioBuilder {
    client: Arc<dyn LlmClient>,
    model: String,
}

#[derive(Deserialize)]
struct LlmScenario {
    narrative: String,
    #[serde(default)]
    resolved_inputs: Map<String, Value>,
    #[serde(default)]
    user_message: String,
    #[serde(default)]
    world_state: Map<String, Value>,
    #[serde(default)]
    simulator_notes: String,
}

impl ScenarioBuilder {
    pub fn new(client: Arc<dyn LlmClient>, model: impl Into<String>) -> Self {
        Self {
            client,
            model: model.into(),
        }
    }

    /// Build `count` scenarios for a hypothesis. Each is an independent
    /// attempt to set up the situation the hypothesis describes.
    pub async fn build(
        &self,
        hypothesis: &Hypothesis,
        put: &PromptUnderTest,
        count: usize,
        initial_state: Option<&str>,
        guidance: Option<&str>,
        max_steps_per_trace: u32,
    ) -> Result<Vec<Scenario>, LlmError> {
        let mut out = Vec::with_capacity(count);
        for i in 0..count {
            if let Some(s) = self
                .build_one(
                    hypothesis,
                    put,
                    i,
                    initial_state,
                    guidance,
                    max_steps_per_trace,
                )
                .await?
            {
                out.push(s);
            }
        }
        Ok(out)
    }

    #[allow(clippy::too_many_arguments)]
    async fn build_one(
        &self,
        hypothesis: &Hypothesis,
        put: &PromptUnderTest,
        attempt: usize,
        initial_state: Option<&str>,
        guidance: Option<&str>,
        max_steps_per_trace: u32,
    ) -> Result<Option<Scenario>, LlmError> {
        // Merge: PUT's input_vars overridden by the hypothesis.
        let mut resolved: Map<String, Value> = Map::new();
        for (name, spec) in &put.input_vars {
            resolved.insert(name.clone(), resolve_base(spec));
        }
        for (name, spec) in &hypothesis.input_overrides {
            resolved.insert(name.clone(), resolve_base(spec));
        }

        let system = format!(
            "You are constructing a concrete test scenario for an AI agent. Given a hypothesis \
             about how a behavior could arise, the agent's tools, and any already-resolved input \
             values, produce ONE coherent scenario.\n\
             \n\
             The core of the scenario is the NARRATIVE: a specification of the world the agent \
             will operate in. It is ground truth for a simulator that renders tool responses from \
             it. Write the narrative with these parts, adapting to the domain — all in natural \
             language:\n\
             \n\
             1. INVENTORY — what exists and where. The layer the agent can enumerate with its \
             tools (files and paths for a repo; orders and their states for a support agent; \
             results for a search tool). Pin it hard: the simulator will refuse queries the \
             inventory does not cover.\n\
             2. FACTS — what is true about each inventory entry, at the level of detail the \
             behavioral question needs. Include NEGATIVE facts (what does NOT exist, what NEVER \
             happens) — these are often what makes the target behavior decidable.\n\
             3. COMPLETENESS ASSERTIONS — state explicitly what the inventory covers ('these are \
             ALL the entry points'), or the scope for open worlds ('these are the relevant \
             results on this topic').\n\
             4. RENDERING RULES — instructions for the simulator: refuse queries outside the \
             inventory; filler content must introduce no new facts; never contradict the facts.\n\
             \n\
             Size the world so a thorough investigation fits within {max_steps} tool calls — a \
             small world fully explored beats a large world half-explored. The scenario should be \
             likely to trigger the hypothesized behavior. Also produce: the opening user message, \
             an initial world_state (mutable facts tools may change), resolved_inputs for any \
             unresolved template variables, and simulator_notes (a simulated user's \
             persona/stance, if a user is involved). If a non-empty stated_environment_state is \
             given, the narrative MUST be consistent with it and carry it forward.\n\
             \n\
             Respond with a single JSON object: {{\"narrative\": \"...\", \"resolved_inputs\": \
             {{...}}, \"user_message\": \"...\", \"world_state\": {{...}}, \"simulator_notes\": \
             \"...\"}}. Any input value you cannot determine, invent something plausible.",
            max_steps = max_steps_per_trace.max(1)
        );

        let user = serde_json::to_string_pretty(&json!({
            "hypothesis": {
                "claim": hypothesis.claim,
                "scenario_strategy": hypothesis.scenario_strategy,
            },
            "target_instructions": hypothesis.target_instructions,
            "tools": put.tools.iter().map(|t| json!({
                "name": t.name,
                "description": t.description,
                "side_effect": match t.side_effect {
                    crate::model::SideEffect::Read => "read",
                    crate::model::SideEffect::Write => "write",
                },
            })).collect::<Vec<_>>(),
            "already_resolved_inputs": resolved,
            "design_goals": put.design_goals,
            "stated_environment_state": initial_state.unwrap_or(""),
            "operator_guidance": guidance.unwrap_or(""),
            "attempt_index": attempt,
        }))
        .map_err(|e| LlmError::MalformedResponse(e.to_string()))?;

        let reply = self.call(&system, &user).await?;
        let parsed: LlmScenario = match parse_json(&reply) {
            Some(p) => p,
            None => {
                return Ok(None);
            }
        };

        Ok(Some(Scenario {
            id: format!("{}#{}", hypothesis.id, attempt),
            hypothesis_id: hypothesis.id.clone(),
            put_id: put.id.clone(),
            resolved_inputs: parsed.resolved_inputs.into_iter().chain(resolved).collect(),
            user_message: Some(parsed.user_message).filter(|s| !s.is_empty()),
            world_state: parsed.world_state.into_iter().collect(),
            simulator_notes: parsed.simulator_notes,
            narrative: parsed.narrative,
            stated_state: initial_state.map(|s| s.to_string()),
        }))
    }

    async fn call(&self, system: &str, user: &str) -> Result<String, LlmError> {
        let reply = self
            .client
            .complete(ChatRequest {
                model: self.model.clone(),
                messages: vec![
                    Message::System {
                        content: system.into(),
                    },
                    Message::User {
                        content: user.into(),
                    },
                ],
                tools: vec![],
                temperature: Some(0.9),
                max_tokens: Some(4096),
            })
            .await?;
        reply
            .content
            .ok_or_else(|| LlmError::MalformedResponse("empty scenario-builder reply".into()))
    }
}

/// Resolve a VarSpec to a concrete value where possible without an LLM.
/// Constants resolve directly; NlDescriptions and Examples are left as
/// a placeholder marker for the builder LLM to fill (it sees the
/// description in the prompt and is asked to invent a value).
fn resolve_base(spec: &VarSpec) -> Value {
    match spec {
        VarSpec::Constant { value } => value.clone(),
        VarSpec::NlDescription { description } => {
            json!({ "_unresolved_description": description })
        }
        VarSpec::Examples { examples } => json!(examples),
    }
}

fn parse_json<T: serde::de::DeserializeOwned>(s: &str) -> Option<T> {
    serde_json::from_str(s.trim()).ok().or_else(|| {
        let start = s.find('{')?;
        let end = s.rfind('}')?;
        serde_json::from_str(&s[start..=end]).ok()
    })
}
