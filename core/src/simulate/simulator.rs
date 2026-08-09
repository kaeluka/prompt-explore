//! The simulator LLM: answers tool calls with realistic responses and
//! proposes world-state patches. It runs as *one persistent conversation
//! per trace*: every tool call is a user message and every reply an
//! assistant turn, so the model looks back at what it already established
//! rather than the harness trying to foresee what future calls will need.
//! Code applies patches; the LLM only proposes. When a reply is unusable,
//! the repair is a conversation message ("your previous reply could not be
//! used: …"), not a parsing branch in code.

use std::sync::Arc;

use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::llm::{ChatRequest, LlmClient, LlmError, Message, parse_json};
use crate::model::ToolSchema;
use crate::model::simulation::ToolCall;

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

/// One simulator conversation for one trace; owns the chat history.
pub struct SimSession {
    client: Arc<dyn LlmClient>,
    model: String,
    messages: Vec<Message>,
}

impl ToolSimulator {
    pub fn new(client: Arc<dyn LlmClient>, model: impl Into<String>) -> Self {
        Self {
            client,
            model: model.into(),
        }
    }

    /// Start a simulator conversation for one scenario trace. The world
    /// specification (narrative + notes) is given once, up front; from
    /// then on the conversation itself is the record of what exists.
    pub fn session(&self, notes: &str) -> SimSession {
        let system = format!(
            "You are simulating software tools inside an agent test harness. \
             Each user message describes one tool call; reply to each with a \
             single JSON object — the tool's response — and nothing else.\n\n\
             Your earlier replies in this conversation are the established \
             record of the environment: every response MUST be consistent \
             with them (same files, same contents, same facts — what has \
             been read stays read).\n\n\
             The WORLD SPECIFICATION below is ground truth: render responses \
             consistent with it, refuse queries for things it says do not \
             exist or that its inventory does not cover, and never introduce \
             facts that contradict it. Filler for unspecified content must \
             introduce no new facts.\n\n\
             WORLD SPECIFICATION AND NOTES:\n{notes}"
        );
        SimSession {
            client: self.client.clone(),
            model: self.model.clone(),
            messages: vec![Message::System { content: system }],
        }
    }
}

impl SimSession {
    pub async fn respond(
        &mut self,
        tool: &ToolSchema,
        call: &ToolCall,
        world_state: &Map<String, Value>,
    ) -> Result<SimOutcome, LlmError> {
        let is_write = matches!(tool.side_effect, crate::model::SideEffect::Write);

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
            "output_format": { "response": "<the tool's return value, any JSON>", "state_patch": "<writes only>" },
            "instructions": write_instructions,
        }))
        .map_err(|e| LlmError::MalformedResponse(e.to_string()))?;

        self.messages.push(Message::User { content: user });

        // Up to three attempts: the initial reply plus up to two repair
        // turns. Repair is conversational — the model is told what was
        // wrong and answers again. If repairs run out, the scenario fails
        // loudly with the raw reply preserved for the operator.
        let mut last_failure = String::new();
        let mut last_raw = String::new();
        for attempt in 0..3 {
            if attempt > 0 {
                // NOTE: interleaved system messages are fine on
                // OpenAI-compatible providers (z.ai, OpenRouter). On
                // providers whose API has a single top-level system param
                // (Bedrock/Anthropic), genai merges system messages, so
                // this repair note loses its position but not its content.
                self.messages.push(Message::System {
                    content: format!(
                        "Your previous reply could not be used: {last_failure}. \
                         Reply again with a single JSON object of the form \
                         {{\"response\": <the tool's return value>, \
                         \"state_patch\": <write calls only>}} and nothing else."
                    ),
                });
            }

            let reply = self
                .client
                .complete(ChatRequest {
                    model: self.model.clone(),
                    messages: self.messages.clone(),
                    tools: vec![],
                    temperature: Some(0.7),
                    // Reasoning-style models can burn a small budget on
                    // hidden reasoning and return empty content.
                    max_tokens: Some(8192),
                })
                .await
                .map_err(|e| LlmError::Provider(e.to_string()))?;

            match reply.content.filter(|c| !c.trim().is_empty()) {
                None => {
                    last_failure = "reply was empty".into();
                }
                Some(content) => match parse_json::<SimReply>(&content) {
                    Some(parsed) => {
                        self.messages.push(Message::Assistant {
                            content: Some(content),
                            tool_calls: vec![],
                        });
                        return Ok(SimOutcome {
                            response: parsed.response,
                            state_patch: if is_write { parsed.state_patch } else { None },
                        });
                    }
                    None => {
                        // Keep the malformed reply in the history so the
                        // repair turn can see exactly what went wrong.
                        self.messages.push(Message::Assistant {
                            content: Some(content.clone()),
                            tool_calls: vec![],
                        });
                        last_raw = content;
                        last_failure =
                            "reply was not a single JSON object of the required shape".into();
                    }
                },
            }
        }

        Err(LlmError::MalformedResponse(format!(
            "simulator reply unusable after repair attempts ({last_failure}): {last_raw}"
        )))
    }
}

#[derive(Deserialize)]
struct SimReply {
    response: Value,
    state_patch: Option<Map<String, Value>>,
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
