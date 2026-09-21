//! The runner: executes one scenario definition against one PUT.
//!
//! Loop: render template → call PUT model → if no tool call, stop.
//! Otherwise render each call through the shared `SimEngine` (supplied Lua
//! first, simulator LLM for whatever it declines), apply state patches in
//! code, and continue. Deterministic bookkeeping; LLMs only for semantics.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use serde_json::Value;

use crate::llm::{ChatRequest, LlmClient, LlmError, Message, ThinkingLevel, ToolDef};
use crate::model::simulation::{
    BudgetCutoffCompletion, RunPhase, RunProgress, RunStopReason, ToolCall, ToolExchange, Trace,
    TraceTurn,
};
use crate::model::{Budget, PromptUnderTest, ToolSchema};

use super::engine::{
    ScenarioRuntime, SimEngine, escaped_placeholders, finish_progress, missing_input_domains,
    parsed_args, summarized_args,
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

pub(crate) struct AgentLoopConfig {
    pub client: Arc<dyn LlmClient>,
    pub model: String,
    pub thinking_level: Option<ThinkingLevel>,
    pub tools: Vec<ToolDef>,
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
    pub max_steps: u64,
    pub max_tokens_total: Option<u64>,
    pub exposed_tool_names: Option<HashSet<String>>,
}

pub(crate) struct AgentLoopOutcome {
    pub turns: Vec<TraceTurn>,
    pub stop_reason: RunStopReason,
    pub output: Option<String>,
    pub budget_cutoff_completion: Option<BudgetCutoffCompletion>,
}

pub(crate) async fn execute_agent_loop(
    sim: &mut SimEngine,
    progress: &Option<Arc<Mutex<RunProgress>>>,
    messages: &mut Vec<Message>,
    config: AgentLoopConfig,
    steps_used: &mut u64,
    tokens_used: &mut u64,
) -> Result<AgentLoopOutcome, RunnerError> {
    let mut turns = Vec::new();
    let mut final_output = None;
    let mut budget_cutoff_completion = None;
    // Earlier stages retain these artifacts in their own invocation records.
    // Do not attribute a recovered failure/cutoff to the next fresh conversation.
    if let Some(progress) = progress {
        if let Ok(mut progress) = progress.lock() {
            progress.execution.budget_cutoff_completion = None;
            progress.execution.unrendered_call = None;
        }
    }
    let stop_reason = loop {
        if *steps_used >= config.max_steps {
            break RunStopReason::StepBudget;
        }
        let response = match config
            .client
            .complete(ChatRequest {
                model: config.model.clone(),
                messages: messages.clone(),
                tools: config.tools.clone(),
                temperature: config.temperature,
                max_tokens: config.max_tokens,
                thinking_level: config.thinking_level,
            })
            .await
        {
            Ok(response) => response,
            Err(error) => return Err(RunnerError::PutModel(error)),
        };

        if let Some(u) = response.usage {
            *tokens_used += u.input_tokens + u.output_tokens;
            if let Some(p) = progress {
                if let Ok(mut g) = p.lock() {
                    g.set_put_tokens_used(*tokens_used);
                }
            }
            if config
                .max_tokens_total
                .is_some_and(|max| *tokens_used > max)
            {
                let cutoff = BudgetCutoffCompletion {
                    model_output: response.content.clone(),
                    thinking: response.thinking.clone(),
                    tool_calls: response.tool_calls.clone(),
                };
                if let Some(p) = progress {
                    if let Ok(mut current) = p.lock() {
                        current.execution.budget_cutoff_completion = Some(cutoff.clone());
                    }
                }
                budget_cutoff_completion = Some(cutoff);
                break RunStopReason::TokenBudget;
            }
        }

        messages.push(Message::Assistant {
            content: response.content.clone(),
            tool_calls: response.tool_calls.clone(),
        });

        if response.tool_calls.is_empty() {
            let model_output = response.content.clone().unwrap_or_default();
            final_output = Some(model_output.clone());
            turns.push(TraceTurn {
                model_output,
                thinking: response.thinking.clone(),
                tool_exchanges: Vec::new(),
            });
            *steps_used += 1;
            if let Some(p) = progress {
                if let Ok(mut g) = p.lock() {
                    g.set_steps_used(*steps_used);
                    g.push_turn(turns.last().unwrap().clone());
                }
            }
            break RunStopReason::FinalCompletion;
        }

        let mut tool_exchanges = Vec::with_capacity(response.tool_calls.len());
        for tc in &response.tool_calls {
            let rendered = if config
                .exposed_tool_names
                .as_ref()
                .is_some_and(|names| !names.contains(&tc.name))
            {
                super::engine::EngineCall {
                    response: Value::String(format!(
                        "error: tool '{}' was not exposed to this invocation",
                        tc.name
                    )),
                    state_after: None,
                    workspace_ops: Vec::new(),
                    sim_thinking: None,
                    lua_execution: None,
                }
            } else {
                match sim.call(&tc.name, &tc.arguments).await {
                    Ok(rendered) => rendered,
                    Err(error) => {
                        if !tool_exchanges.is_empty() {
                            if let Some(p) = progress {
                                if let Ok(mut g) = p.lock() {
                                    g.push_turn(TraceTurn {
                                        model_output: response.content.clone().unwrap_or_default(),
                                        thinking: response.thinking.clone(),
                                        tool_exchanges,
                                    });
                                }
                            }
                        }
                        let call = ToolCall {
                            name: tc.name.clone(),
                            args: parsed_args(tc),
                        };
                        if let Some(p) = progress {
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
            *steps_used += 1;
            if let Some(p) = progress {
                if let Ok(mut g) = p.lock() {
                    g.set_steps_used(*steps_used);
                    g.record_lua_outcome(
                        tool_exchanges
                            .last()
                            .and_then(|exchange| exchange.lua_execution.as_ref())
                            .map(|record| record.outcome),
                    );
                }
            }
        }
        turns.push(TraceTurn {
            model_output: response.content.clone().unwrap_or_default(),
            thinking: response.thinking.clone(),
            tool_exchanges,
        });
        if let Some(p) = progress {
            if let Ok(mut g) = p.lock() {
                g.push_turn(turns.last().unwrap().clone());
            }
        }
    };

    Ok(AgentLoopOutcome {
        turns,
        stop_reason,
        output: if stop_reason == RunStopReason::FinalCompletion {
            final_output
        } else {
            None
        },
        budget_cutoff_completion,
    })
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
            let escaped = escaped_placeholders(&put.template);
            let hint = if escaped.is_empty() {
                String::new()
            } else {
                format!(
                    ". The prompt also escapes {} — escaped braces are literal text too",
                    escaped
                        .iter()
                        .map(|name| format!("'\\{{{{{name}}}}}'"))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            };
            return Err(RunnerError::Mismatch(format!(
                "prompt template uses {} with no input_domain entry in the scenario. To write \
                 literal braces in the prompt text, escape them: \\{{{{name}}}} renders as the \
                 literal text {{{{name}}}} (in a JSON string that is \\\\{{{{name}}}}){hint}",
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
        let mut steps_used = 0u64;
        let mut tokens_used: u64 = 0;
        let outcome = match execute_agent_loop(
            &mut sim,
            &progress,
            &mut messages,
            AgentLoopConfig {
                client: self.put_client.clone(),
                model: self.put_model.clone(),
                thinking_level: self.put_thinking_level,
                tools,
                temperature: self.options.put_temperature,
                max_tokens: self.options.put_max_tokens,
                max_steps: budget.max_steps_per_trace as u64,
                max_tokens_total: budget.max_tokens,
                exposed_tool_names: None,
            },
            &mut steps_used,
            &mut tokens_used,
        )
        .await
        {
            Ok(outcome) => outcome,
            Err(error) => {
                finish_progress(&progress, RunStopReason::RuntimeFailure);
                return Err(error);
            }
        };

        let stop_reason = outcome.stop_reason;
        let turns = outcome.turns;
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
            workflow: None,
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
///
/// A backslash immediately before the braces (`\{{name}}`) escapes them: the
/// backslash is dropped and the braces are emitted literally, never
/// substituted. Substitution is a single left-to-right pass, so a resolved
/// value is never re-scanned for placeholders.
pub(crate) fn render_template(template: &str, vars: &HashMap<String, Value>) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find("{{") {
        let escaped = rest[..start].ends_with('\\');
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else {
            break;
        };
        let name = after[..end].trim();
        let named = !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_');
        if !named {
            // Not a placeholder shape (for example literal JSON braces):
            // copy it through untouched.
            out.push_str(&rest[..start + 2 + end + 2]);
        } else if escaped {
            // Drop the escaping backslash, keep the braces as text.
            out.push_str(&rest[..start - 1]);
            out.push_str(&rest[start..start + 2 + end + 2]);
        } else {
            out.push_str(&rest[..start]);
            match vars.get(name) {
                Some(Value::String(s)) => out.push_str(s),
                Some(other) => out.push_str(&other.to_string()),
                // Undeclared placeholders are rejected before any model call;
                // keeping the text here only serves direct library callers.
                None => out.push_str(&rest[start..start + 2 + end + 2]),
            }
        }
        rest = &after[end + 2..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod render_template_tests {
    use super::render_template;
    use serde_json::{Value, json};
    use std::collections::HashMap;

    fn vars() -> HashMap<String, Value> {
        HashMap::from([("tier".to_string(), json!("gold"))])
    }

    #[test]
    fn substitutes_declared_placeholders_and_leaves_literal_json_braces() {
        assert_eq!(
            render_template("tier {{tier}} and {\"a\": 1}", &vars()),
            "tier gold and {\"a\": 1}"
        );
    }

    #[test]
    fn an_escaped_placeholder_renders_literal_braces() {
        assert_eq!(
            render_template("write \\{{tier}} for the placeholder", &vars()),
            "write {{tier}} for the placeholder"
        );
        // A template with nothing but escaped braces still loses its backslashes.
        assert_eq!(
            render_template("only \\{{tier}}", &HashMap::new()),
            "only {{tier}}"
        );
    }

    #[test]
    fn a_substituted_value_is_never_rescanned() {
        let vars = HashMap::from([("tier".to_string(), json!("{{tier}}"))]);
        assert_eq!(render_template("{{tier}}", &vars), "{{tier}}");
    }
}

fn convert_tool(t: &ToolSchema) -> ToolDef {
    ToolDef {
        name: t.name.clone(),
        description: t.description.clone(),
        parameters: t.parameters.clone(),
    }
}
