//! The simulator LLM: answers tool calls with realistic responses and
//! proposes world-state patches. It makes a *single* structured call
//! per tool invocation — no tool loop of its own. Code applies patches;
//! the LLM only proposes.

use std::sync::Arc;

use serde::Deserialize;
use serde_json::{json, Map, Value};

use crate::llm::{ChatRequest, LlmClient, LlmError, Message};
use crate::model::simulation::ToolCall;
use crate::model::ToolSchema;

pub struct ToolSimulator {
    client: Arc<dyn LlmClient>,
    model: String,
}

/// What the simulator decided for one tool call.
pub struct SimOutcome {
    /// The value returned to the PUT as the tool's response.
    pub response: Value,
    /// Shallow merge into world state (null deletes a key). Present
    /// only for write-tools.
    pub state_patch: Option<Map<String, Value>>,
}

#[derive(Deserialize)]
struct SimReply {
    response: Value,
    state_patch: Option<Map<String, Value>>,
}

impl ToolSimulator {
    pub fn new(client: Arc<dyn LlmClient>, model: impl Into<String>) -> Self {
        Self {
            client,
            model: model.into(),
        }
    }

    pub async fn respond(
        &self,
        tool: &ToolSchema,
        call: &ToolCall,
        world_state: &Map<String, Value>,
        simulator_notes: &str,
    ) -> Result<SimOutcome, LlmError> {
        let is_write = matches!(tool.side_effect, crate::model::SideEffect::Write);

        let system = "You are simulating a software tool inside an agent test harness. \
                      Given a tool's schema, a concrete call, and the current world state, \
                      produce a realistic tool response. Stay consistent with the world \
                      state and with the scenario notes. Reply with a single JSON object \
                      and nothing else."
            .to_string();

        let write_instructions = if is_write {
            "This is a WRITE tool. Also return \"state_patch\": a JSON object that will be \
             shallow-merged into the world state to reflect the call's effect \
             (a null value deletes a key). If the call fails (e.g. precondition violated), \
             make \"response\" an error object and \"state_patch\" an empty object."
        } else {
            "This is a READ tool. Do not include \"state_patch\"."
        };

        let user = serde_json::to_string_pretty(&json!({
            "tool": {
                "name": tool.name,
                "description": tool.description,
                "parameters": tool.parameters,
                "side_effect": if is_write { "write" } else { "read" },
                "example_responses": tool.example_responses,
            },
            "call_arguments": call.args,
            "world_state": world_state,
            "scenario_notes": simulator_notes,
            "output_format": { "response": "<the tool's return value, any JSON>", "state_patch": "<writes only>" },
            "instructions": write_instructions,
        }))
        .map_err(|e| LlmError::MalformedResponse(e.to_string()))?;

        let reply = self
            .client
            .complete(ChatRequest {
                model: self.model.clone(),
                messages: vec![
                    Message::System { content: system },
                    Message::User { content: user },
                ],
                tools: vec![],
                temperature: Some(0.7),
                max_tokens: Some(2048),
            })
            .await?;

        let content = reply
            .content
            .ok_or_else(|| LlmError::MalformedResponse("empty simulator reply".into()))?;
        let parsed: SimReply = parse_json(&content)
            .ok_or_else(|| LlmError::MalformedResponse(format!("simulator reply not JSON: {content}")))?;

        Ok(SimOutcome {
            response: parsed.response,
            state_patch: if is_write { parsed.state_patch } else { None },
        })
    }
}

/// Parse JSON, tolerating surrounding prose or code fences.
fn parse_json(s: &str) -> Option<SimReply> {
    serde_json::from_str(s.trim()).ok().or_else(|| {
        let start = s.find('{')?;
        let end = s.rfind('}')?;
        serde_json::from_str(&s[start..=end]).ok()
    })
}

/// Shallow-merge a patch into world state; null values delete keys.
pub fn apply_patch(state: &mut Map<String, Value>, patch: Map<String, Value>) {
    for (k, v) in patch {
        if v.is_null() {
            state.remove(&k);
        } else {
            state.insert(k, v);
        }
    }
}
