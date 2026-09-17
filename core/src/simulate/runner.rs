//! The runner: executes one Scenario against one PUT.
//!
//! Loop: render template → call PUT model → if no tool call, stop.
//! Otherwise validate arguments (schema errors are fed back to the
//! model as tool errors, as a real framework would), ask the simulator
//! for a response (+ state patch on writes), apply the patch in code,
//! and continue. Deterministic bookkeeping; LLMs only for semantics.

use std::sync::{Arc, Mutex};

use serde_json::{Map, Value};

use crate::llm::{ChatRequest, LlmClient, LlmError, Message, ThinkingLevel, ToolDef};
use crate::model::simulation::{
    LuaExecutionRecord, RunPhase, RunProgress, Scenario, ToolCall, ToolExchange, Trace, TraceTurn,
};
use crate::model::{Budget, PromptUnderTest, ToolSchema};

use super::simulator::{SimSession, SimulatorOptions, ToolSimulator, apply_patch};
use super::workspace::Workspace;

/// Default sampling temperature for the PUT conversation.
pub const DEFAULT_PUT_TEMPERATURE: f32 = 0.7;
/// Default output-token limit for each PUT completion.
pub const DEFAULT_PUT_MAX_TOKENS: u32 = 32 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum RunnerError {
    #[error("PUT model call failed: {0}")]
    PutModel(#[source] LlmError),
    #[error("simulator call failed: {0}")]
    Simulator(#[source] LlmError),
}

/// Controls for the PUT and simulator conversations. The defaults retain the
/// historic behavior; callers can override every limit at the runner boundary.
#[derive(Debug, Clone)]
pub struct RunnerOptions {
    pub put_temperature: Option<f32>,
    pub put_max_tokens: Option<u32>,
    pub simulator: SimulatorOptions,
}

impl Default for RunnerOptions {
    fn default() -> Self {
        Self {
            put_temperature: Some(DEFAULT_PUT_TEMPERATURE),
            put_max_tokens: Some(DEFAULT_PUT_MAX_TOKENS),
            simulator: SimulatorOptions::default(),
        }
    }
}

pub struct Runner {
    put_client: Arc<dyn LlmClient>,
    put_model: String,
    put_thinking_level: Option<ThinkingLevel>,
    options: RunnerOptions,
    simulator: ToolSimulator,
}

impl Runner {
    pub fn new(
        put_client: Arc<dyn LlmClient>,
        put_model: impl Into<String>,
        put_thinking_level: Option<ThinkingLevel>,
        sim_client: Arc<dyn LlmClient>,
        sim_model: impl Into<String>,
        sim_thinking_level: Option<ThinkingLevel>,
        workspace_seed: Workspace,
        options: RunnerOptions,
    ) -> Self {
        Self {
            put_client,
            put_model: put_model.into(),
            put_thinking_level,
            simulator: ToolSimulator::new(
                sim_client,
                sim_model,
                sim_thinking_level,
                workspace_seed,
                options.simulator.clone(),
            ),
            options,
        }
    }

    pub async fn run(
        &self,
        put: &PromptUnderTest,
        scenario: &Scenario,
        budget: &Budget,
        progress: Option<Arc<Mutex<RunProgress>>>,
    ) -> Result<Trace, RunnerError> {
        // Resolve the template's {{variables}} from the scenario's
        // input_domain — finding concrete inputs is the simulator's
        // job. Empty map when the template has no placeholders.
        // One simulator conversation for the whole trace. The first turn
        // resolves the template's {{variables}} from input_domain — IN
        // this world-briefed conversation, so the picked values are
        // consistent with the world the tools will render against.
        if let Some(p) = &progress {
            if let Ok(mut g) = p.lock() {
                g.set_phase(RunPhase::ResolvingInputs);
                g.user_message = scenario.user_message.clone();
            }
        }
        let mut sim = self.simulator.session(&build_simulator_notes(scenario));
        sim.set_progress(progress.clone());
        let resolved_inputs = sim
            .resolve(&put.template, &scenario.input_domain)
            .await
            .map_err(RunnerError::Simulator)?;
        // Surface the resolved bindings immediately (before step 1) so
        // they're visible live, even if preparation or the PUT loop fails.
        if let Some(p) = &progress {
            if let Ok(mut g) = p.lock() {
                g.set_resolved(resolved_inputs.clone());
            }
        }
        sim.prepare_program(&put.tools)
            .await
            .map_err(RunnerError::Simulator)?;
        if let Some(p) = &progress {
            if let Ok(mut g) = p.lock() {
                g.set_phase(RunPhase::PutLoop);
            }
        }
        let mut messages = initial_messages(put, scenario, &resolved_inputs);
        let tools: Vec<ToolDef> = put.tools.iter().map(convert_tool).collect();
        // World state starts empty — initial state is described in the
        // `world` prose, not a structured input. Write-tools mutate this
        // during the trace.
        let mut world_state: Map<String, Value> = Map::new();
        let mut turns = Vec::new();
        let mut steps_used = 0usize;
        let mut tokens_used: u64 = 0;

        loop {
            if steps_used >= budget.max_steps_per_trace as usize {
                break;
            }

            let response = self
                .put_client
                .complete(ChatRequest {
                    model: self.put_model.clone(),
                    messages: messages.clone(),
                    tools: tools.clone(),
                    temperature: self.options.put_temperature,
                    max_tokens: self.options.put_max_tokens,
                    thinking_level: self.put_thinking_level,
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
                turns.push(TraceTurn {
                    model_output: response.content.clone().unwrap_or_default(),
                    thinking: response.thinking.clone(),
                    tool_exchanges: Vec::new(),
                });
                if let Some(p) = &progress {
                    if let Ok(mut g) = p.lock() {
                        g.push_turn(turns.last().unwrap().clone());
                    }
                }
                break;
            }

            // Tool calls emitted by one completion are one atomic batch. Keep
            // them nested in the same trace turn and simulate them in provider
            // order. We finish the whole accepted batch even when it crosses
            // the step cap; splitting it would leave declared tool calls
            // without responses and produce a protocol-incoherent trace.
            let mut tool_exchanges = Vec::with_capacity(response.tool_calls.len());
            for tc in &response.tool_calls {
                let (tool_response, state_after, workspace_ops, sim_thinking, lua_execution) =
                    match self
                        .handle_tool_call(put, tc, &mut world_state, &mut messages, &mut sim)
                        .await
                    {
                        Ok(exchange) => exchange,
                        Err(error) => {
                            // A sibling may have already completed (and, for a
                            // write, patched state) before this call failed.
                            // There is no terminal Trace on error, so retain
                            // that real prefix as ONE partial completion in
                            // live progress. Do not fabricate an exchange for
                            // the failed sibling or split the model completion.
                            if !tool_exchanges.is_empty() {
                                if let Some(p) = &progress {
                                    if let Ok(mut g) = p.lock() {
                                        g.push_turn(TraceTurn {
                                            model_output: response
                                                .content
                                                .clone()
                                                .unwrap_or_default(),
                                            thinking: response.thinking.clone(),
                                            tool_exchanges,
                                        });
                                    }
                                }
                            }
                            return Err(error);
                        }
                    };

                tool_exchanges.push(ToolExchange {
                    call: ToolCall {
                        name: tc.name.clone(),
                        args: serde_json::from_str(&tc.arguments)
                            .unwrap_or(Value::String(tc.arguments.clone())),
                    },
                    response: tool_response,
                    lua_execution,
                    sim_thinking,
                    world_state_after: state_after,
                    workspace_ops,
                });
            }
            steps_used += tool_exchanges.len();
            turns.push(TraceTurn {
                model_output: response.content.clone().unwrap_or_default(),
                thinking: response.thinking.clone(),
                tool_exchanges,
            });
            if let Some(p) = &progress {
                if let Ok(mut g) = p.lock() {
                    g.push_turn(turns.last().unwrap().clone());
                }
            }
        }

        Ok(Trace {
            simulation_program: sim.simulation_program().cloned(),
            turns,
            final_world_state: world_state.into_iter().collect(),
            resolved_inputs,
        })
    }

    /// Validates the call, gets a simulated response, applies any
    /// state patch, and appends the tool message. Returns the tool
    /// response and (for writes) the resulting world state.
    async fn handle_tool_call(
        &self,
        put: &PromptUnderTest,
        tc: &crate::llm::ToolCallRequest,
        world_state: &mut Map<String, Value>,
        messages: &mut Vec<Message>,
        sim: &mut SimSession,
    ) -> Result<
        (
            Value,
            Option<std::collections::HashMap<String, Value>>,
            Vec<crate::model::simulation::WorkspaceOp>,
            Option<String>,
            Option<LuaExecutionRecord>,
        ),
        RunnerError,
    > {
        let tool = put.tools.iter().find(|t| t.name == tc.name);
        let mut workspace_ops = Vec::new();
        let mut sim_thinking = None;
        let mut lua_execution = None;

        let outcome: Value = match tool {
            None => Value::String(format!("error: unknown tool '{}'", tc.name)),
            Some(tool) => match validate_args(tool, &tc.arguments) {
                Err(err) => format!("error: invalid arguments: {err}").into(),
                Ok(args) => {
                    let sim_outcome = sim
                        .respond(
                            tool,
                            &ToolCall {
                                name: tc.name.clone(),
                                args,
                            },
                            world_state,
                        )
                        .await
                        .map_err(RunnerError::Simulator)?;

                    if let Some(patch) = sim_outcome.state_patch {
                        apply_patch(world_state, patch);
                    }
                    workspace_ops = sim_outcome.workspace_ops;
                    sim_thinking = sim_outcome.thinking;
                    lua_execution = sim_outcome.lua_execution;
                    sim_outcome.response
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
        Ok((
            outcome,
            state_after,
            workspace_ops,
            sim_thinking,
            lua_execution,
        ))
    }
}

/// Assemble the simulator's context: persona/stance notes and the world
/// specification (ground truth the simulator renders from, refusing
/// queries outside its inventory). All verbatim; nothing compiled or
/// enforced.
fn build_simulator_notes(scenario: &Scenario) -> String {
    let mut parts: Vec<String> = Vec::new();
    if !scenario.simulator_notes.trim().is_empty() {
        parts.push(scenario.simulator_notes.clone());
    }
    if !scenario.world.trim().is_empty() {
        parts.push(format!(
            "WORLD SPECIFICATION (ground truth for this environment — \
             render responses from it; refuse queries its inventory does \
             not cover; never contradict its facts or introduce new ones \
             in filler): {}",
            scenario.world
        ));
    }
    parts.join("\n\n")
}

fn initial_messages(
    put: &PromptUnderTest,
    scenario: &Scenario,
    resolved_inputs: &std::collections::HashMap<String, Value>,
) -> Vec<Message> {
    let mut msgs = vec![Message::System {
        content: render_template(&put.template, resolved_inputs),
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
