//! The runner: executes one scenario definition against one PUT.
//!
//! Loop: render template → call PUT model → if no tool call, stop.
//! Otherwise render each call through the shared `SimEngine` (supplied Lua
//! first, simulator LLM for whatever it declines), apply state patches in
//! code, and continue. Deterministic bookkeeping; LLMs only for semantics.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde_json::Value;

use crate::llm::{ChatRequest, LlmClient, LlmError, Message, ThinkingLevel, ToolDef};
use crate::model::simulation::{
    BudgetCutoffCompletion, RunPhase, RunProgress, RunStopReason, ToolCall, ToolExchange, Trace,
    TraceTurn,
};
use crate::model::{Budget, PromptUnderTest, ToolSchema};

use super::engine::{
    ScenarioRuntime, SimEngine, finish_progress, missing_input_domains, parsed_args,
    summarized_args,
};

/// Default sampling temperature for the PUT conversation.
pub const DEFAULT_PUT_TEMPERATURE: f32 = 0.7;
/// Default output-token limit for each PUT completion.
pub const DEFAULT_PUT_MAX_TOKENS: u32 = 32 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum RunnerError {
    #[error("PUT model call failed: {0}")]
    PutModel(#[source] LlmError),
    /// Empty `context` for input-resolution failures; the failing tool request
    /// when a tool response could not be rendered, so the failure text alone
    /// identifies which call has no response.
    #[error("simulator call failed{context}: {source}")]
    Simulator {
        context: String,
        #[source]
        source: LlmError,
    },
    #[error("scenario/prompt mismatch: {0}")]
    Mismatch(String),
}

/// Controls for the PUT conversation. The defaults retain the historic
/// behavior; callers can override every limit at the runner boundary.
///
/// Simulator controls live in the SCENARIO definition (`SimulationSettings`),
/// because the environment is part of the test case — not in the investigation.
#[derive(Debug, Clone)]
pub struct RunnerOptions {
    pub put_temperature: Option<f32>,
    pub put_max_tokens: Option<u32>,
}

impl Default for RunnerOptions {
    fn default() -> Self {
        Self {
            put_temperature: Some(DEFAULT_PUT_TEMPERATURE),
            put_max_tokens: Some(DEFAULT_PUT_MAX_TOKENS),
        }
    }
}

pub struct Runner {
    put_client: Arc<dyn LlmClient>,
    put_model: String,
    put_thinking_level: Option<ThinkingLevel>,
    sim_client: Arc<dyn LlmClient>,
    sim_model: String,
    sim_thinking_level: Option<ThinkingLevel>,
    options: RunnerOptions,
}

impl Runner {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        put_client: Arc<dyn LlmClient>,
        put_model: impl Into<String>,
        put_thinking_level: Option<ThinkingLevel>,
        sim_client: Arc<dyn LlmClient>,
        sim_model: impl Into<String>,
        sim_thinking_level: Option<ThinkingLevel>,
        options: RunnerOptions,
    ) -> Self {
        Self {
            put_client,
            put_model: put_model.into(),
            put_thinking_level,
            sim_client,
            sim_model: sim_model.into(),
            sim_thinking_level,
            options,
        }
    }

    /// Run the prompt under test inside one scenario. `resolved_inputs`, when
    /// given, pins the scenario's declared inputs instead of sampling them.
    pub async fn run(
        &self,
        put: &PromptUnderTest,
        runtime: &ScenarioRuntime,
        budget: &Budget,
        resolved_inputs: Option<&HashMap<String, Value>>,
        progress: Option<Arc<Mutex<RunProgress>>>,
    ) -> Result<Trace, RunnerError> {
        let scenario = &runtime.scenario;
        // Always retain progress internally. This makes direct Runner callers
        // produce the same execution evidence as investigator-driven jobs.
        let progress =
            Some(progress.unwrap_or_else(|| Arc::new(Mutex::new(RunProgress::default()))));
        if let Some(p) = &progress {
            if let Ok(mut g) = p.lock() {
                g.ensure_initialized(scenario.user_message.clone());
            }
        }
        // A prompt placeholder the scenario does not declare is a caller
        // mistake worth naming before any model call.
        let missing = missing_input_domains(&put.template, &scenario.input_domain);
        if !missing.is_empty() {
            finish_progress(&progress, RunStopReason::RuntimeFailure);
            return Err(RunnerError::Mismatch(format!(
                "prompt template uses {} with no input_domain entry in the scenario",
                missing
                    .iter()
                    .map(|name| format!("'{{{{{name}}}}}'"))
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        }
        if let Some(p) = &progress {
            if let Ok(mut g) = p.lock() {
                g.set_phase(RunPhase::ResolvingInputs);
                g.user_message = scenario.user_message.clone();
                g.set_implementations(runtime.implementations.clone());
            }
        }
        // One simulator session for the whole trace. The first turn resolves
        // the scenario's declared inputs IN this world-briefed conversation, so
        // the picked values are consistent with the world (or the caller's
        // explicit bindings are used verbatim).
        let mut sim = match SimEngine::start(
            self.sim_client.clone(),
            self.sim_model.clone(),
            self.sim_thinking_level,
            runtime,
            resolved_inputs,
            progress.clone(),
        )
        .await
        {
            Ok(sim) => sim,
            Err(error) => {
                finish_progress(&progress, RunStopReason::RuntimeFailure);
                return Err(RunnerError::Simulator {
                    context: String::new(),
                    source: error,
                });
            }
        };
        // Surface the resolved bindings immediately (before step 1) so
        // they're visible live, even if the PUT loop fails.
        let resolved_inputs = sim.resolved_inputs().clone();
        if let Some(p) = &progress {
            if let Ok(mut g) = p.lock() {
                g.set_resolved(resolved_inputs.clone());
                g.set_phase(RunPhase::PutLoop);
            }
        }
        let mut messages = initial_messages(put, scenario, &resolved_inputs);
        let tools: Vec<ToolDef> = runtime.tools.iter().map(convert_tool).collect();
        let mut turns = Vec::new();
        let mut steps_used = 0u64;
        let mut tokens_used: u64 = 0;
        let stop_reason = loop {
            if steps_used >= budget.max_steps_per_trace as u64 {
                break RunStopReason::StepBudget;
            }
            let response = match self
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
            {
                Ok(response) => response,
                Err(error) => {
                    finish_progress(&progress, RunStopReason::RuntimeFailure);
                    return Err(RunnerError::PutModel(error));
                }
            };

            if let Some(u) = response.usage {
                tokens_used += u.input_tokens + u.output_tokens;
                if let Some(p) = &progress {
                    if let Ok(mut g) = p.lock() {
                        g.set_put_tokens_used(tokens_used);
                    }
                }
                if budget.max_tokens.is_some_and(|max| tokens_used > max) {
                    if let Some(p) = &progress {
                        if let Ok(mut current) = p.lock() {
                            current.execution.budget_cutoff_completion =
                                Some(BudgetCutoffCompletion {
                                    model_output: response.content.clone(),
                                    thinking: response.thinking.clone(),
                                    tool_calls: response.tool_calls.clone(),
                                });
                        }
                    }
                    break RunStopReason::TokenBudget;
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
                steps_used += 1;
                if let Some(p) = &progress {
                    if let Ok(mut g) = p.lock() {
                        g.set_steps_used(steps_used);
                        g.push_turn(turns.last().unwrap().clone());
                    }
                }
                break RunStopReason::FinalCompletion;
            }

            // Tool calls emitted by one completion are one atomic batch. Keep
            // them nested in the same trace turn and simulate them in provider
            // order. We finish the whole accepted batch even when it crosses
            // the step cap; splitting it would leave declared tool calls
            // without responses and produce a protocol-incoherent trace.
            let mut tool_exchanges = Vec::with_capacity(response.tool_calls.len());
            for tc in &response.tool_calls {
                let rendered = match sim.call(&tc.name, &tc.arguments).await {
                    Ok(rendered) => rendered,
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
                                        model_output: response.content.clone().unwrap_or_default(),
                                        thinking: response.thinking.clone(),
                                        tool_exchanges,
                                    });
                                }
                            }
                        }
                        finish_progress(&progress, RunStopReason::RuntimeFailure);
                        let call = ToolCall {
                            name: tc.name.clone(),
                            args: parsed_args(tc),
                        };
                        if let Some(p) = &progress {
                            if let Ok(mut g) = p.lock() {
                                g.execution.unrendered_call = Some(call.clone());
                            }
                        }
                        return Err(RunnerError::Simulator {
                            context: format!(
                                " for tool '{}' (args {})",
                                call.name,
                                summarized_args(&call.args)
                            ),
                            source: error,
                        });
                    }
                };
                messages.push(Message::Tool {
                    tool_call_id: tc.id.clone(),
                    content: rendered.response.to_string(),
                });
                tool_exchanges.push(ToolExchange {
                    call: ToolCall {
                        name: tc.name.clone(),
                        args: parsed_args(tc),
                    },
                    response: rendered.response,
                    lua_execution: rendered.lua_execution,
                    sim_thinking: rendered.sim_thinking,
                    world_state_after: rendered.state_after,
                    workspace_ops: rendered.workspace_ops,
                });
                // Count each successfully completed sibling immediately. The
                // trace remains one atomic batch, but a later sibling failure
                // must not erase evidence of work already performed.
                steps_used += 1;
                if let Some(p) = &progress {
                    if let Ok(mut g) = p.lock() {
                        g.set_steps_used(steps_used);
                    }
                }
            }
            // The accepted sibling batch remains one trace turn even when it
            // crossed the cap; the cap is checked before the next completion.
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
        };

        finish_progress(&progress, stop_reason);
        let execution = progress
            .as_ref()
            .and_then(|p| p.lock().ok().map(|g| g.snapshot().execution))
            .unwrap_or_default();
        Ok(Trace {
            execution,
            implementations: runtime.implementations.clone(),
            turns,
            final_world_state: sim.world_state().clone().into_iter().collect(),
            resolved_inputs,
        })
    }
}

fn initial_messages(
    put: &PromptUnderTest,
    scenario: &crate::model::Scenario,
    resolved_inputs: &HashMap<String, Value>,
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
fn render_template(template: &str, vars: &HashMap<String, Value>) -> String {
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
