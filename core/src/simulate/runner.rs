//! The runner: executes one Scenario against one PUT.
//!
//! Loop: render template → call PUT model → if no tool call, stop.
//! Otherwise validate arguments (schema errors are fed back to the
//! model as tool errors, as a real framework would), ask the simulator
//! for a response (+ state patch on writes), apply the patch in code,
//! and continue. Deterministic bookkeeping; LLMs only for semantics.

use std::sync::Arc;

use serde_json::{Map, Value};

use crate::llm::{ChatRequest, LlmClient, LlmError, Message, ToolDef};
use crate::model::simulation::{Scenario, ToolCall, Trace, TraceStep};
use crate::model::{Budget, PromptUnderTest, ToolSchema};

use super::simulator::{ToolSimulator, apply_patch};

#[derive(Debug, thiserror::Error)]
pub enum RunnerError {
    #[error("PUT model call failed: {0}")]
    PutModel(#[source] LlmError),
    #[error("simulator call failed: {0}")]
    Simulator(#[source] LlmError),
}

pub struct Runner {
    put_client: Arc<dyn LlmClient>,
    put_model: String,
    simulator: ToolSimulator,
}

impl Runner {
    pub fn new(
        put_client: Arc<dyn LlmClient>,
        put_model: impl Into<String>,
        sim_client: Arc<dyn LlmClient>,
        sim_model: impl Into<String>,
    ) -> Self {
        Self {
            put_client,
            put_model: put_model.into(),
            simulator: ToolSimulator::new(sim_client, sim_model),
        }
    }

    pub async fn run(
        &self,
        put: &PromptUnderTest,
        scenario: &Scenario,
        budget: &Budget,
    ) -> Result<Trace, RunnerError> {
        let mut messages = initial_messages(put, scenario);
        let tools: Vec<ToolDef> = put.tools.iter().map(convert_tool).collect();
        let mut world_state: Map<String, Value> =
            scenario.world_state.clone().into_iter().collect();
        let mut steps = Vec::new();
        let mut tokens_used: u64 = 0;

        loop {
            if steps.len() >= budget.max_steps_per_trace as usize {
                break;
            }

            let response = self
                .put_client
                .complete(ChatRequest {
                    model: self.put_model.clone(),
                    messages: messages.clone(),
                    tools: tools.clone(),
                    temperature: Some(0.7),
                    max_tokens: None,
                })
                .await
                .map_err(RunnerError::PutModel)?;

            if let Some(u) = response.usage {
                tokens_used += u.input_tokens + u.output_tokens;
                if budget.max_tokens.is_some_and(|max| tokens_used > max) {
                    break;
                }
            }

            messages.push(Message::Assistant {
                content: response.content.clone(),
                tool_calls: response.tool_calls.clone(),
            });

            if response.tool_calls.is_empty() {
                steps.push(TraceStep {
                    model_output: response.content.unwrap_or_default(),
                    tool_call: None,
                    tool_response: None,
                    world_state_after: None,
                });
                break;
            }

            // A response may carry several tool calls; each becomes its
            // own step so the trace reads as a linear story.
            for (i, tc) in response.tool_calls.iter().enumerate() {
                let (tool_response, state_after) = self
                    .handle_tool_call(put, scenario, tc, &mut world_state, &mut messages)
                    .await?;

                steps.push(TraceStep {
                    model_output: if i == 0 {
                        response.content.clone().unwrap_or_default()
                    } else {
                        String::new()
                    },
                    tool_call: Some(ToolCall {
                        name: tc.name.clone(),
                        args: serde_json::from_str(&tc.arguments)
                            .unwrap_or(Value::String(tc.arguments.clone())),
                    }),
                    tool_response: Some(tool_response),
                    world_state_after: state_after,
                });

                if steps.len() >= budget.max_steps_per_trace as usize {
                    break;
                }
            }
        }

        Ok(Trace {
            scenario_id: scenario.id.clone(),
            steps,
            final_world_state: world_state.into_iter().collect(),
            verdict: None,
        })
    }

    /// Validates the call, gets a simulated response, applies any
    /// state patch, and appends the tool message. Returns the tool
    /// response and (for writes) the resulting world state.
    async fn handle_tool_call(
        &self,
        put: &PromptUnderTest,
        scenario: &Scenario,
        tc: &crate::llm::ToolCallRequest,
        world_state: &mut Map<String, Value>,
        messages: &mut Vec<Message>,
    ) -> Result<(Value, Option<std::collections::HashMap<String, Value>>), RunnerError> {
        let tool = put.tools.iter().find(|t| t.name == tc.name);

        let outcome: Value = match tool {
            None => Value::String(format!("error: unknown tool '{}'", tc.name)),
            Some(tool) => match validate_args(tool, &tc.arguments) {
                Err(err) => format!("error: invalid arguments: {err}").into(),
                Ok(args) => {
                    // The user-stated environment state rides the same
                    // notes channel the scenario already uses — verbatim,
                    // not compiled or enforced. The simulator is asked to
                    // respect it; when it doesn't, that's visible in the
                    // trace and the judge (which also sees the state) can
                    // catch the inconsistency.
                    let notes = match &scenario.stated_state {
                        Some(s) => format!(
                            "{}\n\nUSER-SPECIFIED ENVIRONMENT STATE (the operator requires \
                             this of the environment — respect it exactly): {}",
                            scenario.simulator_notes, s
                        ),
                        None => scenario.simulator_notes.clone(),
                    };
                    let sim = self
                        .simulator
                        .respond(
                            tool,
                            &ToolCall {
                                name: tc.name.clone(),
                                args,
                            },
                            world_state,
                            &notes,
                        )
                        .await
                        .map_err(RunnerError::Simulator)?;

                    if let Some(patch) = sim.state_patch {
                        apply_patch(world_state, patch);
                    }
                    sim.response
                }
            },
        };

        messages.push(Message::Tool {
            tool_call_id: tc.id.clone(),
            content: outcome.to_string(),
        });

        let state_after = match tool.map(|t| &t.side_effect) {
            Some(crate::model::SideEffect::Write) => {
                Some(world_state.clone().into_iter().collect())
            }
            _ => None,
        };
        Ok((outcome, state_after))
    }
}

fn initial_messages(put: &PromptUnderTest, scenario: &Scenario) -> Vec<Message> {
    let mut msgs = vec![Message::System {
        content: render_template(&put.template, &scenario.resolved_inputs),
    }];
    if let Some(user) = &scenario.user_message {
        msgs.push(Message::User {
            content: user.clone(),
        });
    }
    msgs
}

/// Minimal `{{var}}` substitution. Strings are inserted raw, other
/// JSON values in their serialized form.
fn render_template(template: &str, vars: &std::collections::HashMap<String, Value>) -> String {
    let mut out = template.to_string();
    for (k, v) in vars {
        let replacement = match v {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        out = out.replace(&format!("{{{{{k}}}}}"), &replacement);
    }
    out
}

fn convert_tool(t: &ToolSchema) -> ToolDef {
    ToolDef {
        name: t.name.clone(),
        description: t.description.clone(),
        parameters: t.parameters.clone(),
    }
}

fn validate_args(tool: &ToolSchema, arguments: &str) -> Result<Value, String> {
    let args: Value =
        serde_json::from_str(arguments).map_err(|e| format!("arguments not JSON: {e}"))?;
    let validator = jsonschema::validator_for(&tool.parameters)
        .map_err(|e| format!("invalid tool schema: {e}"))?;
    validator.validate(&args).map_err(|e| e.to_string())?;
    Ok(args)
}
