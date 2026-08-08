//! Scenario building: hypothesis + PUT → one or more concrete scenarios.
//!
//! Resolves input_vars (constants copied, NlDescriptions made concrete,
//! hypothesis input_overrides applied), invents a coherent initial
//! world_state from the tool schemas, and writes the user_message and
//! simulator_notes that embody the hypothesis.

use std::sync::Arc;

use serde::Deserialize;
use serde_json::{json, Map, Value};

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
    resolved_inputs: Map<String, Value>,
    user_message: String,
    world_state: Map<String, Value>,
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
    ) -> Result<Vec<Scenario>, LlmError> {
        let mut out = Vec::with_capacity(count);
        for i in 0..count {
            if let Some(s) = self.build_one(hypothesis, put, i, initial_state).await? {
                out.push(s);
            }
        }
        Ok(out)
    }

    async fn build_one(
        &self,
        hypothesis: &Hypothesis,
        put: &PromptUnderTest,
        attempt: usize,
        initial_state: Option<&str>,
    ) -> Result<Option<Scenario>, LlmError> {
        // Merge: PUT's input_vars overridden by the hypothesis.
        let mut resolved: Map<String, Value> = Map::new();
        for (name, spec) in &put.input_vars {
            resolved.insert(name.clone(), resolve_base(spec));
        }
        for (name, spec) in &hypothesis.input_overrides {
            resolved.insert(name.clone(), resolve_base(spec));
        }

        let system = "You are constructing a concrete test scenario for an AI agent. \
                      Given a hypothesis about how a behavior could arise, the agent's \
                      tools, and any already-resolved input values, produce ONE coherent \
                      scenario: an initial world state (consistent with the tool schemas), \
                      the opening user message, and notes guiding how the simulated \
                      environment (including a simulated user, if relevant) should behave. \
                      Make the scenario likely to trigger the hypothesized behavior. \
                      Respond with a single JSON object: {\"resolved_inputs\": {...}, \
                      \"user_message\": \"...\", \"world_state\": {...}, \
                      \"simulator_notes\": \"...\"}. Any input value you cannot determine, \
                      invent something plausible. Keep world_state consistent with what the \
                      tools would read and write. If a non-empty \
                      stated_environment_state is given, your world_state MUST be consistent \
                      with it and your simulator_notes MUST carry it forward so the \
                      simulated environment respects it (e.g. a tool described as broken \
                      keeps returning errors)."
            .to_string();

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
            resolved_inputs: parsed
                .resolved_inputs
                .into_iter()
                .chain(resolved)
                .collect(),
            user_message: Some(parsed.user_message),
            world_state: parsed.world_state.into_iter().collect(),
            simulator_notes: parsed.simulator_notes,
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
                    Message::User { content: user.into() },
                ],
                tools: vec![],
                temperature: Some(0.9),
                max_tokens: Some(2048),
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
