use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use mlua::{
    ChunkMode, Error as LuaError, Lua, LuaOptions as MluaOptions, StdLib, Value as LuaValue,
};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::generate::LlmRole;
use crate::llm::{LlmClient, Message, ToolDef};
use crate::model::scenario::source_hash;
use crate::model::simulation::{RunPhase, RunProgress, RunStopReason, Trace};
use crate::model::workflow::{
    AgentControls, AgentInvocationRecord, DirectToolCallRecord, WorkflowEvidence, WorkflowLimits,
    WorkflowProgram,
};
use crate::model::{Budget, PromptUnderTest, ToolSchema};

use super::engine::{ScenarioRuntime, SimEngine, escaped_placeholders, missing_input_domains};
use super::lua::{
    Budget as LuaBudget, JsonLimits, RunError as LuaRunError, classify_lua_error,
    install_budget_hook, install_workflow_sandbox, json_to_lua, lua_to_json,
};
use super::runner::{AgentLoopConfig, RunnerOptions, execute_agent_loop, render_template};

#[derive(Debug)]
pub(crate) struct WorkflowRunError {
    pub stage: &'static str,
    pub error: String,
}

impl WorkflowRunError {
    fn workflow(error: impl Into<String>) -> Self {
        Self {
            stage: "workflow",
            error: error.into(),
        }
    }

    fn runner(error: impl Into<String>) -> Self {
        Self {
            stage: "runner",
            error: error.into(),
        }
    }
}

pub(crate) async fn run_workflow(
    put_role: LlmRole,
    sim_role: LlmRole,
    runner_options: RunnerOptions,
    investigation_budget: Budget,
    put: PromptUnderTest,
    runtime: ScenarioRuntime,
    resolved_inputs: Option<HashMap<String, Value>>,
    progress: Arc<Mutex<RunProgress>>,
    workflow: WorkflowProgram,
    legacy_mode: bool,
) -> Result<Trace, WorkflowRunError> {
    workflow.validate().map_err(WorkflowRunError::workflow)?;
    if let Ok(mut progress) = progress.lock() {
        progress.workflow = Some(WorkflowEvidence {
            source: workflow.lua_source.clone(),
            source_hash: source_hash(&workflow.lua_source),
            params: workflow.params.clone(),
            limits: Some(workflow.limits.clone()),
            output: None,
            invocations: vec![],
            tool_calls: vec![],
            stop_reason: None,
            error: None,
        });
    }

    let missing = missing_input_domains(&put.template, &runtime.scenario.input_domain);
    if !missing.is_empty() {
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
        return Err(WorkflowRunError::runner(format!(
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

    if let Ok(mut g) = progress.lock() {
        g.ensure_initialized(runtime.scenario.user_message.clone());
        g.set_phase(RunPhase::ResolvingInputs);
        g.user_message = runtime.scenario.user_message.clone();
        g.set_implementations(runtime.implementations.clone());
    }

    let engine = SimEngine::start(
        sim_role.client.clone(),
        sim_role.model.clone(),
        sim_role.thinking_level,
        &runtime,
        resolved_inputs.as_ref(),
        Some(progress.clone()),
    )
    .await
    .map_err(|error| WorkflowRunError::runner(format!("simulator call failed: {error}")))?;

    let resolved = engine.resolved_inputs().clone();
    if let Ok(mut g) = progress.lock() {
        g.set_resolved(resolved.clone());
        g.set_phase(RunPhase::Orchestration);
    }

    let prepared_params = if legacy_mode {
        json!({
            "prompt": render_template(&put.template, &resolved),
            "model": put_role.model,
            "controls": {
                "thinking": put_role.thinking_level,
                "temperature": runner_options.put_temperature,
                "max_tokens": runner_options.put_max_tokens,
            }
        })
    } else {
        workflow.params.clone()
    };

    let initial_evidence = WorkflowEvidence {
        source: workflow.lua_source.clone(),
        source_hash: source_hash(&workflow.lua_source),
        params: prepared_params.clone(),
        limits: Some(workflow.limits.clone()),
        output: None,
        invocations: vec![],
        tool_calls: vec![],
        stop_reason: None,
        error: None,
    };
    if let Ok(mut g) = progress.lock() {
        g.workflow = Some(initial_evidence.clone());
    }

    let host = Arc::new(tokio::sync::Mutex::new(WorkflowHost {
        put_client: put_role.client.clone(),
        default_temperature: runner_options.put_temperature,
        default_max_tokens: runner_options.put_max_tokens,
        runtime: runtime.clone(),
        investigation_budget,
        progress: progress.clone(),
        engine,
        resolved_inputs: resolved.clone(),
        workflow_source: workflow.lua_source.clone(),
        workflow_source_hash: source_hash(&workflow.lua_source),
        workflow_params: prepared_params.clone(),
        workflow_limits: workflow.limits.clone(),
        workflow_output: None,
        workflow_error: None,
        steps_used: 0,
        tokens_used: 0,
        exhausted: None,
        next_event_id: 0,
        next_invocation_id: 0,
        invocations: vec![],
        tool_calls: vec![],
        max_host_calls: workflow.limits.max_host_calls,
        max_host_bytes: workflow.limits.max_host_bytes,
        max_agent_invocations: workflow.limits.max_agent_invocations,
        max_direct_tool_calls: workflow.limits.max_direct_tool_calls,
        host_calls_used: 0,
        host_bytes_used: 0,
        agent_invocations_used: 0,
        direct_tool_calls_used: 0,
        legacy_mode,
    }));

    let lua_output =
        run_lua_program(host.clone(), &workflow, prepared_params.clone(), &resolved).await;

    let mut host_locked = host.lock().await;
    match lua_output {
        Ok(output) => host_locked.workflow_output = output,
        Err(error) => host_locked.workflow_error = Some(error),
    }

    let stop_reason = if let Some(reason) = host_locked.exhausted {
        reason
    } else if host_locked.workflow_error.is_some() {
        RunStopReason::RuntimeFailure
    } else {
        RunStopReason::FinalCompletion
    };
    host_locked.sync_progress(Some(stop_reason));

    let first_invocation_failure = host_locked
        .legacy_mode
        .then(|| {
            host_locked
                .invocations
                .iter()
                .find_map(|invocation| invocation.failure.clone())
        })
        .flatten();

    if !host_locked.invocations.is_empty() {
        if let Ok(mut g) = host_locked.progress.lock() {
            g.set_phase(RunPhase::PutLoop);
        }
    }

    if let Some(error) = first_invocation_failure {
        return Err(WorkflowRunError::runner(error));
    }
    if let Some(error) = &host_locked.workflow_error {
        return Err(WorkflowRunError::workflow(error.clone()));
    }
    if let Some(reason) = host_locked.exhausted {
        if let Ok(mut g) = host_locked.progress.lock() {
            g.finish(reason);
        }
    }
    if let Ok(mut g) = host_locked.progress.lock() {
        g.finish(stop_reason);
    }

    let execution = host_locked
        .progress
        .lock()
        .ok()
        .map(|g| g.snapshot().execution)
        .unwrap_or_default();
    let turns = host_locked
        .progress
        .lock()
        .ok()
        .map(|g| g.turns.clone())
        .unwrap_or_default();
    let workflow_evidence = Some(host_locked.workflow_evidence(Some(stop_reason)));
    Ok(Trace {
        execution,
        implementations: host_locked.runtime.implementations.clone(),
        turns,
        final_world_state: host_locked
            .engine
            .world_state()
            .clone()
            .into_iter()
            .collect(),
        resolved_inputs: host_locked.resolved_inputs.clone(),
        workflow: workflow_evidence,
    })
}

struct WorkflowHost {
    put_client: Arc<dyn LlmClient>,
    default_temperature: Option<f32>,
    default_max_tokens: Option<u32>,
    runtime: ScenarioRuntime,
    investigation_budget: Budget,
    progress: Arc<Mutex<RunProgress>>,
    engine: SimEngine,
    resolved_inputs: HashMap<String, Value>,
    workflow_source: String,
    workflow_source_hash: String,
    workflow_params: Value,
    workflow_limits: WorkflowLimits,
    workflow_output: Option<Value>,
    workflow_error: Option<String>,
    steps_used: u64,
    tokens_used: u64,
    exhausted: Option<RunStopReason>,
    next_event_id: u64,
    next_invocation_id: u64,
    invocations: Vec<AgentInvocationRecord>,
    tool_calls: Vec<DirectToolCallRecord>,
    max_host_calls: usize,
    max_host_bytes: usize,
    max_agent_invocations: usize,
    max_direct_tool_calls: usize,
    host_calls_used: usize,
    host_bytes_used: usize,
    agent_invocations_used: usize,
    direct_tool_calls_used: usize,
    legacy_mode: bool,
}

impl WorkflowHost {
    fn workflow_evidence(&self, stop_reason: Option<RunStopReason>) -> WorkflowEvidence {
        WorkflowEvidence {
            source: self.workflow_source.clone(),
            source_hash: self.workflow_source_hash.clone(),
            params: self.workflow_params.clone(),
            limits: Some(self.workflow_limits.clone()),
            output: self.workflow_output.clone(),
            invocations: self.invocations.clone(),
            tool_calls: self.tool_calls.clone(),
            stop_reason,
            error: self.workflow_error.clone(),
        }
    }

    fn sync_progress(&self, stop_reason: Option<RunStopReason>) {
        if let Ok(mut g) = self.progress.lock() {
            g.workflow = Some(self.workflow_evidence(stop_reason));
            g.execution.steps_used = self.steps_used;
            g.execution.put_tokens_used = self.tokens_used;
        }
    }

    fn next_event_id(&mut self) -> u64 {
        let current = self.next_event_id;
        self.next_event_id += 1;
        current
    }

    fn charge_host_call(&mut self, args: &Value) -> Result<(), WorkflowRunError> {
        if self.host_calls_used >= self.max_host_calls {
            self.exhausted = Some(RunStopReason::RuntimeFailure);
            return Err(WorkflowRunError::workflow(
                "workflow host-call limit exceeded",
            ));
        }
        let arg_bytes = serialized_bytes(args)?;
        if arg_bytes > self.max_host_bytes.saturating_sub(self.host_bytes_used) {
            self.exhausted = Some(RunStopReason::RuntimeFailure);
            return Err(WorkflowRunError::workflow(
                "workflow host-call byte limit exceeded",
            ));
        }
        self.host_calls_used += 1;
        self.host_bytes_used += arg_bytes;
        Ok(())
    }

    fn charge_host_result(&mut self, result: &Value) -> Result<(), WorkflowRunError> {
        let result_bytes = serialized_bytes(result)?;
        if result_bytes > self.max_host_bytes.saturating_sub(self.host_bytes_used) {
            self.exhausted = Some(RunStopReason::RuntimeFailure);
            return Err(WorkflowRunError::workflow(
                "workflow host-call byte limit exceeded",
            ));
        }
        self.host_bytes_used += result_bytes;
        Ok(())
    }

    fn global_step_exhausted(&self) -> bool {
        self.steps_used >= self.investigation_budget.max_steps_per_trace as u64
    }

    fn global_token_exhausted(&self) -> bool {
        self.investigation_budget
            .max_tokens
            .is_some_and(|max| self.tokens_used > max)
    }

    fn mark_budget_if_exhausted(&mut self) {
        if self.global_token_exhausted() {
            self.exhausted = Some(RunStopReason::TokenBudget);
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RunAgentRequest {
    #[serde(default)]
    name: Option<String>,
    prompt: String,
    model: String,
    #[serde(default)]
    input: Option<String>,
    #[serde(default)]
    tools: Option<Vec<String>>,
    #[serde(default)]
    controls: Option<RunAgentControls>,
    #[serde(default)]
    budget: Option<RunAgentBudget>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RunAgentControls {
    thinking: Option<crate::llm::ThinkingLevel>,
    temperature: Option<f32>,
    max_tokens: Option<u32>,
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RunAgentBudget {
    max_steps: Option<u32>,
    max_tokens: Option<u64>,
}

impl Default for RunAgentBudget {
    fn default() -> Self {
        Self {
            max_steps: None,
            max_tokens: None,
        }
    }
}

fn validate_run_agent_request(
    request: &RunAgentRequest,
    legacy_mode: bool,
) -> Result<(), WorkflowRunError> {
    if !legacy_mode && !request.model.contains("::") {
        return Err(WorkflowRunError::workflow(
            "ctx.run_agent model must be provider-qualified (expected provider::model)",
        ));
    }
    if !request.prompt.is_empty() || legacy_mode {
        // prompt is required by serde; only validate remaining runtime properties here.
    }
    if let Some(controls) = &request.controls {
        if let Some(temperature) = controls.temperature {
            if !temperature.is_finite() || temperature < 0.0 {
                return Err(WorkflowRunError::workflow(
                    "ctx.run_agent controls.temperature must be finite and nonnegative",
                ));
            }
        }
        if controls.max_tokens.is_some_and(|max| max == 0) {
            return Err(WorkflowRunError::workflow(
                "ctx.run_agent controls.max_tokens must be greater than zero",
            ));
        }
    }
    if let Some(budget) = &request.budget {
        if budget.max_steps.is_some_and(|steps| steps == 0) {
            return Err(WorkflowRunError::workflow(
                "ctx.run_agent budget.max_steps must be greater than zero",
            ));
        }
        if budget.max_tokens.is_some_and(|tokens| tokens == 0) {
            return Err(WorkflowRunError::workflow(
                "ctx.run_agent budget.max_tokens must be greater than zero",
            ));
        }
    }
    Ok(())
}

async fn run_lua_program(
    host: Arc<tokio::sync::Mutex<WorkflowHost>>,
    workflow: &WorkflowProgram,
    params: Value,
    resolved_inputs: &HashMap<String, Value>,
) -> Result<Option<Value>, String> {
    let limits = workflow.limits.clone();
    let lua_options = workflow_lua_options(&limits);
    let lua_budget = LuaBudget::new(&lua_options);
    let lua = Lua::new_with(StdLib::ALL_SAFE, MluaOptions::default())
        .map_err(|error| format!("could not create Lua VM: {error}"))?;
    lua.set_memory_limit(limits.max_memory_bytes)
        .map_err(|error| format!("could not set Lua memory limit: {error}"))?;
    install_budget_hook(&lua, lua_budget.clone());
    let json = install_workflow_sandbox(&lua).map_err(lua_run_error_to_string)?;

    let params_limits = JsonLimits::new(&lua_options, lua_budget.clone());
    let params_value = json_to_lua(&lua, &params, &json, &params_limits, 0)
        .map_err(|error| format!("params conversion failed: {error}"))?;
    params_limits
        .finish()
        .map_err(|error| format!("params conversion failed: {error}"))?;

    let ctx = lua
        .create_table()
        .map_err(|error| format!("ctx table creation failed: {error}"))?;
    let input_value = match host.lock().await.runtime.scenario.user_message.clone() {
        Some(input) => Value::String(input),
        None => Value::Null,
    };
    let ctx_limits = JsonLimits::new(&lua_options, lua_budget.clone());
    let input_lua = json_to_lua(&lua, &input_value, &json, &ctx_limits, 0)
        .map_err(|error| format!("ctx.input conversion failed: {error}"))?;
    let resolved_lua = json_to_lua(
        &lua,
        &serde_json::to_value(resolved_inputs).expect("resolved inputs JSON"),
        &json,
        &ctx_limits,
        0,
    )
    .map_err(|error| format!("ctx.resolved_inputs conversion failed: {error}"))?;
    ctx.set("input", input_lua)
        .map_err(|error| format!("ctx.input set failed: {error}"))?;
    ctx.set("resolved_inputs", resolved_lua)
        .map_err(|error| format!("ctx.resolved_inputs set failed: {error}"))?;

    let run_agent_json = json.clone();
    let run_agent_budget = lua_budget.clone();
    let run_agent_options = lua_options.clone();
    let run_agent_host = host.clone();
    let run_agent = lua
        .create_async_function(move |lua, args: LuaValue| {
            let run_agent_json = run_agent_json.clone();
            let run_agent_budget = run_agent_budget.clone();
            let run_agent_options = run_agent_options.clone();
            let run_agent_host = run_agent_host.clone();
            async move {
                let limits = JsonLimits::new(&run_agent_options, run_agent_budget.clone());
                let mut args_json = lua_to_json(args, &run_agent_json, &limits, 0)?;
                limits.finish()?;
                if let Some(tools) = args_json.get_mut("tools") {
                    if matches!(tools, Value::Object(map) if map.is_empty()) {
                        *tools = Value::Array(vec![]);
                    }
                }
                let request: RunAgentRequest =
                    serde_json::from_value(args_json.clone()).map_err(|error| {
                        LuaError::RuntimeError(format!("ctx.run_agent options invalid: {error}"))
                    })?;
                run_agent_budget.pause()?;
                let result = run_agent_host_call(run_agent_host.clone(), request, args_json).await;
                run_agent_budget.resume();
                match result {
                    Ok(result) => {
                        let out_limits =
                            JsonLimits::new(&run_agent_options, run_agent_budget.clone());
                        let result_lua =
                            json_to_lua(&lua, &result, &run_agent_json, &out_limits, 0)?;
                        out_limits.finish()?;
                        Ok(result_lua)
                    }
                    Err(error) => Err(LuaError::RuntimeError(error.error)),
                }
            }
        })
        .map_err(|error| format!("ctx.run_agent creation failed: {error}"))?;
    ctx.set("run_agent", run_agent)
        .map_err(|error| format!("ctx.run_agent set failed: {error}"))?;

    let call_tool_json = json.clone();
    let call_tool_budget = lua_budget.clone();
    let call_tool_options = lua_options.clone();
    let call_tool_host = host.clone();
    let call_tool = lua
        .create_async_function(move |lua, (name, args): (String, LuaValue)| {
            let call_tool_json = call_tool_json.clone();
            let call_tool_budget = call_tool_budget.clone();
            let call_tool_options = call_tool_options.clone();
            let call_tool_host = call_tool_host.clone();
            async move {
                let limits = JsonLimits::new(&call_tool_options, call_tool_budget.clone());
                let args_json = lua_to_json(args, &call_tool_json, &limits, 0)?;
                limits.finish()?;
                call_tool_budget.pause()?;
                let result = direct_tool_host_call(call_tool_host.clone(), name, args_json).await;
                call_tool_budget.resume();
                match result {
                    Ok(value) => {
                        let out_limits =
                            JsonLimits::new(&call_tool_options, call_tool_budget.clone());
                        let value = json_to_lua(&lua, &value, &call_tool_json, &out_limits, 0)?;
                        out_limits.finish()?;
                        Ok(value)
                    }
                    Err(error) => Err(LuaError::RuntimeError(error.error)),
                }
            }
        })
        .map_err(|error| format!("ctx.call_tool creation failed: {error}"))?;
    ctx.set("call_tool", call_tool)
        .map_err(|error| format!("ctx.call_tool set failed: {error}"))?;

    let handler: LuaValue = lua
        .load(&workflow.lua_source)
        .set_mode(ChunkMode::Text)
        .eval_async()
        .await
        .map_err(|error| format!("workflow load/eval failed: {}", lua_error_to_string(error)))?;
    let function = match handler {
        LuaValue::Function(function) => function,
        _ => {
            return Err(
                "workflow source does not return a function; write `return function(params, ctx) ... end`".into(),
            )
        }
    };

    let result: LuaValue = function
        .call_async((params_value, ctx))
        .await
        .map_err(|error| format!("workflow call failed: {}", lua_error_to_string(error)))?;
    if matches!(result, LuaValue::Nil) {
        return Ok(None);
    }
    let output_limits = JsonLimits::new(&lua_options, lua_budget.clone());
    let value = lua_to_json(result, &json, &output_limits, 0).map_err(lua_error_to_string)?;
    output_limits.finish().map_err(lua_error_to_string)?;
    Ok(Some(value))
}

async fn run_agent_host_call(
    host: Arc<tokio::sync::Mutex<WorkflowHost>>,
    request: RunAgentRequest,
    args_json: Value,
) -> Result<Value, WorkflowRunError> {
    let mut host = host.lock().await;
    if let Some(reason) = host.exhausted {
        return Err(WorkflowRunError::workflow(format!(
            "global investigation budget already exhausted ({reason:?})"
        )));
    }
    if host.global_step_exhausted() {
        host.exhausted = Some(RunStopReason::StepBudget);
        return Err(WorkflowRunError::workflow(
            "global investigation step budget exhausted",
        ));
    }
    if host.global_token_exhausted() {
        host.exhausted = Some(RunStopReason::TokenBudget);
        return Err(WorkflowRunError::workflow(
            "global investigation token budget exhausted",
        ));
    }
    host.charge_host_call(&args_json)?;
    if host.agent_invocations_used >= host.max_agent_invocations {
        host.exhausted = Some(RunStopReason::RuntimeFailure);
        return Err(WorkflowRunError::workflow(
            "workflow agent-invocation limit exceeded",
        ));
    }
    validate_run_agent_request(&request, host.legacy_mode)?;

    let tool_names = resolve_tool_subset(&host.runtime.tools, request.tools.as_ref())
        .map_err(WorkflowRunError::workflow)?;
    let tools: Vec<ToolDef> = host
        .runtime
        .tools
        .iter()
        .filter(|tool| tool_names.contains(&tool.name))
        .map(convert_tool)
        .collect();

    let invocation_id = host.next_invocation_id;
    host.next_invocation_id += 1;
    host.agent_invocations_used += 1;
    let event_id = host.next_event_id();
    let turn_start = host
        .progress
        .lock()
        .ok()
        .map(|g| g.turns.len())
        .unwrap_or(0);
    let controls = AgentControls {
        thinking: request
            .controls
            .as_ref()
            .and_then(|controls| controls.thinking),
        temperature: request
            .controls
            .as_ref()
            .and_then(|controls| controls.temperature)
            .or(host.default_temperature),
        max_tokens: request
            .controls
            .as_ref()
            .and_then(|controls| controls.max_tokens)
            .or(host.default_max_tokens),
    };
    let input = request.input.clone();
    let name = request.name.clone().unwrap_or_else(|| "agent".into());
    let record_index = host.invocations.len();
    host.invocations.push(AgentInvocationRecord {
        event_id,
        invocation_id,
        name: name.clone(),
        prompt: request.prompt.clone(),
        model: request.model.clone(),
        input: input.clone(),
        tools: tools.iter().map(|tool| tool.name.clone()).collect(),
        controls: controls.clone(),
        budget: None,
        turn_start,
        turn_end: turn_start,
        steps_used: 0,
        tokens_used: 0,
        stop_reason: None,
        running: true,
        output: None,
        failure: None,
        budget_cutoff_completion: None,
        unrendered_call: None,
    });
    if let Ok(mut g) = host.progress.lock() {
        g.set_phase(RunPhase::PutLoop);
    }
    host.sync_progress(None);

    let invocation_steps_before = host.steps_used;
    let invocation_tokens_before = host.tokens_used;
    let messages_input = input.clone();
    let mut messages = vec![Message::System {
        content: request.prompt.clone(),
    }];
    if let Some(input) = &messages_input {
        messages.push(Message::User {
            content: input.clone(),
        });
    }
    let invocation_budget = request.budget.unwrap_or_default();
    let invocation_max_steps = host.steps_used.saturating_add(
        invocation_budget
            .max_steps
            .map(|steps| steps as u64)
            .unwrap_or(u64::MAX),
    );
    let invocation_max_tokens = invocation_budget
        .max_tokens
        .map(|tokens| host.tokens_used.saturating_add(tokens));
    let progress_handle = Some(host.progress.clone());
    let client = host.put_client.clone();
    let global_max_steps = host.investigation_budget.max_steps_per_trace as u64;
    let global_max_tokens = host.investigation_budget.max_tokens;
    let max_steps = global_max_steps.min(invocation_max_steps);
    let max_tokens_total = match (global_max_tokens, invocation_max_tokens) {
        (Some(global), Some(local)) => Some(global.min(local)),
        (Some(global), None) => Some(global),
        (None, Some(local)) => Some(local),
        (None, None) => None,
    };
    let effective_budget = Budget {
        max_steps_per_trace: max_steps.saturating_sub(host.steps_used) as u32,
        max_tokens: max_tokens_total.map(|max| max.saturating_sub(host.tokens_used)),
    };
    host.invocations[record_index].budget = Some(effective_budget);
    host.sync_progress(None);
    let mut steps_used = host.steps_used;
    let mut tokens_used = host.tokens_used;

    let result = execute_agent_loop(
        &mut host.engine,
        &progress_handle,
        &mut messages,
        AgentLoopConfig {
            client,
            model: request.model.clone(),
            thinking_level: controls.thinking,
            tools,
            temperature: controls.temperature,
            max_tokens: controls.max_tokens,
            max_steps,
            max_tokens_total,
            exposed_tool_names: Some(tool_names.clone()),
        },
        &mut steps_used,
        &mut tokens_used,
    )
    .await;
    host.steps_used = steps_used;
    host.tokens_used = tokens_used;
    if let Ok(mut g) = host.progress.lock() {
        g.set_phase(RunPhase::Orchestration);
    }

    let turn_end = host
        .progress
        .lock()
        .ok()
        .map(|g| g.turns.len())
        .unwrap_or(turn_start);
    let (stop_reason, output, failure_text, budget_cutoff_completion, unrendered_call) =
        match result {
            Ok(loop_outcome) => {
                if loop_outcome.stop_reason == RunStopReason::StepBudget
                    && host.global_step_exhausted()
                {
                    host.exhausted = Some(RunStopReason::StepBudget);
                }
                if loop_outcome.stop_reason == RunStopReason::TokenBudget
                    && host.global_token_exhausted()
                {
                    host.exhausted = Some(RunStopReason::TokenBudget);
                }
                host.mark_budget_if_exhausted();
                (
                    Some(loop_outcome.stop_reason),
                    loop_outcome.output,
                    None,
                    loop_outcome.budget_cutoff_completion,
                    None,
                )
            }
            Err(error) => {
                let text = error.to_string();
                let unrendered = host
                    .progress
                    .lock()
                    .ok()
                    .and_then(|g| g.execution.unrendered_call.clone());
                (
                    Some(RunStopReason::RuntimeFailure),
                    None,
                    Some(text),
                    None,
                    unrendered,
                )
            }
        };

    let final_steps_used = host.steps_used.saturating_sub(invocation_steps_before);
    let final_tokens_used = host.tokens_used.saturating_sub(invocation_tokens_before);
    {
        let record = &mut host.invocations[record_index];
        record.turn_end = turn_end;
        record.steps_used = final_steps_used;
        record.tokens_used = final_tokens_used;
        record.stop_reason = stop_reason;
        record.running = false;
        record.output = output.clone();
        record.failure = failure_text.clone();
        record.budget_cutoff_completion = budget_cutoff_completion.clone();
        record.unrendered_call = unrendered_call.clone();
    }
    host.sync_progress(host.exhausted.or(stop_reason));

    let record = &host.invocations[record_index];
    let mut result = serde_json::Map::new();
    result.insert("event_id".into(), json!(event_id));
    result.insert("invocation_id".into(), json!(invocation_id));
    result.insert("name".into(), json!(record.name));
    result.insert("model".into(), json!(record.model));
    result.insert(
        "input".into(),
        serde_json::to_value(&record.input).expect("json"),
    );
    result.insert(
        "tools".into(),
        serde_json::to_value(&record.tools).expect("json"),
    );
    result.insert(
        "controls".into(),
        serde_json::to_value(&record.controls).expect("json"),
    );
    result.insert(
        "stop_reason".into(),
        serde_json::to_value(&record.stop_reason).expect("json"),
    );
    result.insert("budget".into(), json!(record.budget));
    result.insert("steps_used".into(), json!(record.steps_used));
    result.insert("tokens_used".into(), json!(record.tokens_used));
    result.insert(
        "turns".into(),
        json!(record.turn_end.saturating_sub(record.turn_start)),
    );
    if let Some(output) = &record.output {
        result.insert("output".into(), json!(output));
    }
    if let Some(failure) = &record.failure {
        result.insert(
            "failure".into(),
            json!({
                "stage": "runner",
                "error": failure,
                "invocation_id": invocation_id,
                "event_id": event_id,
            }),
        );
    }
    if let Some(cutoff) = &record.budget_cutoff_completion {
        result.insert(
            "budget_cutoff_completion".into(),
            serde_json::to_value(cutoff).expect("json"),
        );
    }
    if let Some(call) = &record.unrendered_call {
        result.insert(
            "unrendered_call".into(),
            serde_json::to_value(call).expect("json"),
        );
    }
    let result = Value::Object(result);
    host.charge_host_result(&result)?;
    Ok(result)
}

async fn direct_tool_host_call(
    host: Arc<tokio::sync::Mutex<WorkflowHost>>,
    name: String,
    args: Value,
) -> Result<Value, WorkflowRunError> {
    let mut host = host.lock().await;
    if let Some(reason) = host.exhausted {
        return Err(WorkflowRunError::workflow(format!(
            "global investigation budget already exhausted ({reason:?})"
        )));
    }
    if host.global_step_exhausted() {
        host.exhausted = Some(RunStopReason::StepBudget);
        return Err(WorkflowRunError::workflow(
            "global investigation step budget exhausted",
        ));
    }
    if host.global_token_exhausted() {
        host.exhausted = Some(RunStopReason::TokenBudget);
        return Err(WorkflowRunError::workflow(
            "global investigation token budget exhausted",
        ));
    }
    if host.direct_tool_calls_used >= host.max_direct_tool_calls {
        host.exhausted = Some(RunStopReason::RuntimeFailure);
        return Err(WorkflowRunError::workflow(
            "workflow direct-tool-call limit exceeded",
        ));
    }
    let charge = json!({"name": name, "args": args});
    host.charge_host_call(&charge)?;
    host.direct_tool_calls_used += 1;
    let event_id = host.next_event_id();
    if let Ok(mut g) = host.progress.lock() {
        g.set_phase(RunPhase::PutLoop);
    }
    let result = host.engine.call_value(&name, args.clone()).await;
    if let Ok(mut g) = host.progress.lock() {
        g.set_phase(RunPhase::Orchestration);
    }
    match result {
        Ok(call) => {
            host.steps_used += 1;
            if let Ok(mut g) = host.progress.lock() {
                g.set_steps_used(host.steps_used);
                g.record_lua_outcome(call.lua_execution.as_ref().map(|record| record.outcome));
            }
            let record = DirectToolCallRecord {
                event_id,
                name: name.clone(),
                args: args.clone(),
                response: Some(call.response.clone()),
                state_after: call.state_after.clone(),
                workspace_ops: call.workspace_ops.clone(),
                lua_execution: call.lua_execution.clone(),
                sim_thinking: call.sim_thinking.clone(),
                failure: None,
                running: false,
            };
            host.tool_calls.push(record);
            host.mark_budget_if_exhausted();
            host.sync_progress(host.exhausted.or(Some(RunStopReason::FinalCompletion)));
            host.charge_host_result(&call.response)?;
            Ok(call.response)
        }
        Err(error) => {
            let text = error.to_string();
            host.tool_calls.push(DirectToolCallRecord {
                event_id,
                name,
                args,
                response: None,
                state_after: None,
                workspace_ops: vec![],
                lua_execution: None,
                sim_thinking: None,
                failure: Some(text.clone()),
                running: false,
            });
            host.sync_progress(Some(RunStopReason::RuntimeFailure));
            Err(WorkflowRunError::runner(text))
        }
    }
}

fn resolve_tool_subset(
    all: &[ToolSchema],
    subset: Option<&Vec<String>>,
) -> Result<HashSet<String>, String> {
    let available: HashSet<String> = all.iter().map(|tool| tool.name.clone()).collect();
    match subset {
        None => Ok(available),
        Some(requested) => {
            let mut out = HashSet::new();
            for name in requested {
                if !available.contains(name) {
                    return Err(format!(
                        "unknown tool '{name}' in ctx.run_agent tools subset"
                    ));
                }
                out.insert(name.clone());
            }
            Ok(out)
        }
    }
}

fn convert_tool(t: &ToolSchema) -> ToolDef {
    ToolDef {
        name: t.name.clone(),
        description: t.description.clone(),
        parameters: t.parameters.clone(),
    }
}

fn lua_run_error_to_string(error: LuaRunError) -> String {
    match error {
        LuaRunError::Fallback(detail) | LuaRunError::Failed(detail) => detail,
    }
}

fn lua_error_to_string(error: LuaError) -> String {
    match classify_lua_error(error) {
        LuaRunError::Fallback(detail) | LuaRunError::Failed(detail) => detail,
    }
}

fn workflow_lua_options(limits: &WorkflowLimits) -> crate::model::LuaOptions {
    crate::model::LuaOptions {
        max_memory_bytes: limits.max_memory_bytes,
        max_instructions: limits.max_instructions,
        max_host_calls: limits.max_host_calls,
        max_host_bytes: limits.max_host_bytes,
        max_source_bytes: limits.max_source_bytes,
        max_duration_ms: limits.max_duration_ms,
        max_value_depth: limits.max_value_depth,
        max_result_bytes: limits.max_result_bytes,
    }
}

fn serialized_bytes(value: &Value) -> Result<usize, WorkflowRunError> {
    serde_json::to_vec(value)
        .map(|bytes| bytes.len())
        .map_err(|error| WorkflowRunError::workflow(format!("JSON serialization failed: {error}")))
}

fn json_depth(value: &Value, depth: usize) -> usize {
    match value {
        Value::Array(values) => values
            .iter()
            .map(|value| json_depth(value, depth + 1))
            .max()
            .unwrap_or(depth),
        Value::Object(values) => values
            .values()
            .map(|value| json_depth(value, depth + 1))
            .max()
            .unwrap_or(depth),
        _ => depth,
    }
}

impl WorkflowProgram {
    pub fn validate(&self) -> Result<(), String> {
        self.limits.validate()?;
        if self.lua_source.len() > self.limits.max_source_bytes {
            return Err(format!(
                "workflow source is {} bytes; limit is {}",
                self.lua_source.len(),
                self.limits.max_source_bytes
            ));
        }
        let param_bytes = serde_json::to_vec(&self.params)
            .map_err(|error| format!("workflow params do not serialize: {error}"))?
            .len();
        if param_bytes > self.limits.max_result_bytes {
            return Err(format!(
                "workflow params are {param_bytes} bytes; limit is {}",
                self.limits.max_result_bytes
            ));
        }
        if json_depth(&self.params, 0) > self.limits.max_value_depth {
            return Err(format!(
                "workflow params exceed max_value_depth {}",
                self.limits.max_value_depth
            ));
        }
        let lua = Lua::new_with(StdLib::ALL_SAFE, MluaOptions::default())
            .map_err(|error| format!("could not create Lua VM: {error}"))?;
        lua.load(&self.lua_source)
            .set_mode(ChunkMode::Text)
            .into_function()
            .map(|_| ())
            .map_err(|error| format!("workflow source does not parse as Lua: {error}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::{ChatResponse, MockLlmClient};
    use crate::model::{Investigation, Scenario, SideEffect};
    use crate::simulate::Workspace;

    fn investigation() -> Investigation {
        Investigation {
            reason: None,
            budget: Budget {
                max_steps_per_trace: 10,
                max_tokens: None,
            },
        }
    }

    fn put() -> PromptUnderTest {
        PromptUnderTest {
            id: "p".into(),
            template: "System {{item}}".into(),
            tools: vec![ToolSchema {
                name: "store".into(),
                description: "Store a file.".into(),
                parameters: json!({
                    "type":"object",
                    "properties": {"path":{"type":"string"},"content":{"type":"string"}},
                    "required": ["path","content"],
                    "additionalProperties": false
                }),
                side_effect: SideEffect::Write,
                example_responses: vec![],
            }],
            design_goals: String::new(),
        }
    }

    fn scenario() -> Scenario {
        Scenario {
            world: "tool store writes state; nothing else exists".into(),
            input_domain: [("item".into(), "literal value".into())].into(),
            user_message: Some("hello".into()),
            simulator_notes: String::new(),
        }
    }

    fn runtime() -> ScenarioRuntime {
        ScenarioRuntime::from_put(&put(), scenario(), Workspace::empty())
    }

    #[tokio::test(flavor = "current_thread")]
    async fn trivial_workflow_runs_without_host_calls() {
        let trace = run_workflow(
            LlmRole {
                client: Arc::new(MockLlmClient::scripted(vec![])),
                model: "put-model".into(),
                thinking_level: None,
            },
            LlmRole {
                client: Arc::new(MockLlmClient::scripted(vec![ChatResponse {
                    content: Some(r#"{"item":"sample"}"#.into()),
                    thinking: None,
                    tool_calls: vec![],
                    usage: None,
                }])),
                model: "sim-model".into(),
                thinking_level: None,
            },
            RunnerOptions::default(),
            investigation().budget,
            put(),
            runtime(),
            None,
            Arc::new(Mutex::new(RunProgress::default())),
            WorkflowProgram {
                lua_source: "return function(params, ctx) return 1 end".into(),
                params: Value::Null,
                ..Default::default()
            },
            false,
        )
        .await
        .unwrap();
        assert_eq!(trace.workflow.unwrap().output, Some(json!(1)));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn pcall_cannot_bypass_global_step_exhaustion() {
        let workflow = WorkflowProgram {
            lua_source: r#"return function(params, ctx)
  local first = ctx.run_agent({name='one', prompt='p', model='mock::m', input=ctx.input})
  local ok = pcall(function()
    return ctx.run_agent({name='two', prompt='p', model='mock::m', input=ctx.input})
  end)
  return {first = first and first.output or json.null, second_ok = ok}
end"#
                .into(),
            ..Default::default()
        };
        let trace = run_workflow(
            LlmRole {
                client: Arc::new(MockLlmClient::scripted(vec![ChatResponse {
                    content: Some("done".into()),
                    thinking: None,
                    tool_calls: vec![],
                    usage: None,
                }])),
                model: "put-model".into(),
                thinking_level: None,
            },
            LlmRole {
                client: Arc::new(MockLlmClient::scripted(vec![ChatResponse {
                    content: Some(r#"{"item":"sample"}"#.into()),
                    thinking: None,
                    tool_calls: vec![],
                    usage: None,
                }])),
                model: "sim-model".into(),
                thinking_level: None,
            },
            RunnerOptions::default(),
            Budget {
                max_steps_per_trace: 1,
                max_tokens: None,
            },
            put(),
            runtime(),
            None,
            Arc::new(Mutex::new(RunProgress::default())),
            workflow,
            false,
        )
        .await
        .unwrap();
        assert_eq!(trace.execution.stop_reason, Some(RunStopReason::StepBudget));
        let wf = trace.workflow.unwrap();
        assert_eq!(wf.stop_reason, Some(RunStopReason::StepBudget));
        assert_eq!(wf.output.as_ref().unwrap()["first"], "done");
        assert_eq!(wf.output.as_ref().unwrap()["second_ok"], false);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn workflow_sandbox_hides_host_apis_but_keeps_pcall() {
        let trace = run_workflow(
            LlmRole {
                client: Arc::new(MockLlmClient::scripted(vec![])),
                model: "put-model".into(),
                thinking_level: None,
            },
            LlmRole {
                client: Arc::new(MockLlmClient::scripted(vec![ChatResponse {
                    content: Some(r#"{"item":"sample"}"#.into()),
                    thinking: None,
                    tool_calls: vec![],
                    usage: None,
                }])),
                model: "sim-model".into(),
                thinking_level: None,
            },
            RunnerOptions::default(),
            investigation().budget,
            put(),
            runtime(),
            None,
            Arc::new(Mutex::new(RunProgress::default())),
            WorkflowProgram {
                lua_source: r#"return function(params, ctx)
  return {
    io = io == nil,
    os = os == nil,
    package = package == nil,
    debug = debug == nil,
    dofile = dofile == nil,
    loadfile = loadfile == nil,
    require = require == nil,
    pcall = pcall ~= nil,
  }
end"#
                    .into(),
                params: Value::Null,
                ..Default::default()
            },
            false,
        )
        .await
        .unwrap();
        assert_eq!(
            trace.workflow.unwrap().output,
            Some(json!({
                "io": true,
                "os": true,
                "package": true,
                "debug": true,
                "dofile": true,
                "loadfile": true,
                "require": true,
                "pcall": true,
            }))
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn run_agent_validation_and_explicit_limits_are_enforced() {
        let bad_model = run_workflow(
            LlmRole { client: Arc::new(MockLlmClient::scripted(vec![])), model: "put-model".into(), thinking_level: None },
            LlmRole {
                client: Arc::new(MockLlmClient::scripted(vec![ChatResponse {
                    content: Some(r#"{"item":"sample"}"#.into()),
                    thinking: None,
                    tool_calls: vec![],
                    usage: None,
                }])),
                model: "sim-model".into(),
                thinking_level: None,
            },
            RunnerOptions::default(),
            investigation().budget,
            put(),
            runtime(),
            None,
            Arc::new(Mutex::new(RunProgress::default())),
            WorkflowProgram {
                lua_source: r#"return function(params, ctx) return ctx.run_agent({prompt='p', model='bare'}) end"#.into(),
                ..Default::default()
            },
            false,
        )
        .await
        .unwrap_err();
        assert!(bad_model.error.contains("provider-qualified"));

        let too_many_agents = run_workflow(
            LlmRole {
                client: Arc::new(MockLlmClient::scripted(vec![ChatResponse {
                    content: Some("done".into()),
                    thinking: None,
                    tool_calls: vec![],
                    usage: None,
                }])),
                model: "put-model".into(),
                thinking_level: None,
            },
            LlmRole {
                client: Arc::new(MockLlmClient::scripted(vec![ChatResponse {
                    content: Some(r#"{"item":"sample"}"#.into()),
                    thinking: None,
                    tool_calls: vec![],
                    usage: None,
                }])),
                model: "sim-model".into(),
                thinking_level: None,
            },
            RunnerOptions::default(),
            investigation().budget,
            put(),
            runtime(),
            None,
            Arc::new(Mutex::new(RunProgress::default())),
            WorkflowProgram {
                lua_source: r#"return function(params, ctx)
  local a = ctx.run_agent({prompt='p', model='mock::m', input=ctx.input})
  return ctx.run_agent({prompt='p', model='mock::m', input=ctx.input})
end"#
                    .into(),
                limits: WorkflowLimits {
                    max_agent_invocations: 1,
                    ..Default::default()
                },
                ..Default::default()
            },
            false,
        )
        .await
        .unwrap_err();
        assert!(
            too_many_agents
                .error
                .contains("agent-invocation limit exceeded")
        );

        let too_many_tools = run_workflow(
            LlmRole {
                client: Arc::new(MockLlmClient::scripted(vec![])),
                model: "put-model".into(),
                thinking_level: None,
            },
            LlmRole {
                client: Arc::new(MockLlmClient::scripted(vec![
                    ChatResponse {
                        content: Some(r#"{"item":"sample"}"#.into()),
                        thinking: None,
                        tool_calls: vec![],
                        usage: None,
                    },
                    ChatResponse {
                        content: Some(r#"{"response":{"ok":true},"state_patch":{}}"#.into()),
                        thinking: None,
                        tool_calls: vec![],
                        usage: None,
                    },
                ])),
                model: "sim-model".into(),
                thinking_level: None,
            },
            RunnerOptions::default(),
            investigation().budget,
            put(),
            runtime(),
            None,
            Arc::new(Mutex::new(RunProgress::default())),
            WorkflowProgram {
                lua_source: r#"return function(params, ctx)
  ctx.call_tool('store', {path='x', content='1'})
  return ctx.call_tool('store', {path='y', content='2'})
end"#
                    .into(),
                limits: WorkflowLimits {
                    max_direct_tool_calls: 1,
                    ..Default::default()
                },
                ..Default::default()
            },
            false,
        )
        .await
        .unwrap_err();
        assert!(
            too_many_tools
                .error
                .contains("direct-tool-call limit exceeded")
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn workflow_validate_checks_parse_and_param_bounds() {
        assert!(WorkflowProgram::default().validate().is_ok());
        let mut bad = WorkflowProgram::default();
        bad.lua_source = "return {".into();
        assert!(bad.validate().unwrap_err().contains("does not parse"));
        let mut deep = WorkflowProgram::default();
        deep.limits.max_value_depth = 2;
        deep.params = json!([[[1]]]);
        assert!(deep.validate().unwrap_err().contains("max_value_depth"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn default_single_stage_records_workflow_evidence() {
        let trace = run_workflow(
            LlmRole {
                client: Arc::new(MockLlmClient::scripted(vec![ChatResponse {
                    content: Some("done".into()),
                    thinking: None,
                    tool_calls: vec![],
                    usage: None,
                }])),
                model: "put-model".into(),
                thinking_level: None,
            },
            LlmRole {
                client: Arc::new(MockLlmClient::scripted(vec![ChatResponse {
                    content: Some(r#"{"item":"sample"}"#.into()),
                    thinking: None,
                    tool_calls: vec![],
                    usage: None,
                }])),
                model: "sim-model".into(),
                thinking_level: None,
            },
            RunnerOptions::default(),
            investigation().budget,
            put(),
            runtime(),
            None,
            Arc::new(Mutex::new(RunProgress::default())),
            WorkflowProgram::default(),
            true,
        )
        .await
        .unwrap();
        let wf = trace.workflow.unwrap();
        assert_eq!(wf.source, crate::model::workflow::DEFAULT_WORKFLOW_LUA);
        assert_eq!(wf.output, Some(json!("done")));
        assert_eq!(wf.invocations.len(), 1);
        assert_eq!(wf.invocations[0].output.as_deref(), Some("done"));
        assert_eq!(trace.turns.len(), 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn two_stages_handoff_exact_output_and_tool_subset_none() {
        let workflow = WorkflowProgram {
            lua_source: r#"return function(params, ctx)
  local a = ctx.run_agent({name='one', prompt=params.p1, model=params.model, input=ctx.input})
  local b = ctx.run_agent({name='two', prompt=params.p2, model=params.model, input=a.output, tools={}})
  return b.output
end"#
                .into(),
            params: json!({"model":"mock::put-model","p1":"first","p2":"second"}),
            ..Default::default()
        };
        let put_client = Arc::new(MockLlmClient::scripted(vec![
            ChatResponse {
                content: Some("facts".into()),
                thinking: None,
                tool_calls: vec![],
                usage: None,
            },
            ChatResponse {
                content: Some("answer".into()),
                thinking: None,
                tool_calls: vec![],
                usage: None,
            },
        ]));
        let sim_client = Arc::new(MockLlmClient::scripted(vec![ChatResponse {
            content: Some(r#"{"item":"sample"}"#.into()),
            thinking: None,
            tool_calls: vec![],
            usage: None,
        }]));
        let trace = run_workflow(
            LlmRole {
                client: put_client.clone(),
                model: "legacy-unused".into(),
                thinking_level: None,
            },
            LlmRole {
                client: sim_client,
                model: "sim-model".into(),
                thinking_level: None,
            },
            RunnerOptions::default(),
            investigation().budget,
            put(),
            runtime(),
            None,
            Arc::new(Mutex::new(RunProgress::default())),
            workflow,
            false,
        )
        .await
        .unwrap();
        let reqs = put_client.requests.lock().unwrap();
        assert_eq!(reqs.len(), 2);
        match &reqs[1].messages[1] {
            Message::User { content } => assert_eq!(content, "facts"),
            other => panic!("expected user handoff, got {other:?}"),
        }
        assert!(reqs[1].tools.is_empty());
        let wf = trace.workflow.unwrap();
        assert_eq!(wf.invocations.len(), 2);
        assert_eq!(wf.output, Some(json!("answer")));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn default_source_in_custom_workflow_honors_supplied_params() {
        let put_client = Arc::new(MockLlmClient::scripted(vec![ChatResponse {
            content: Some("done".into()),
            thinking: None,
            tool_calls: vec![],
            usage: None,
        }]));
        let trace = run_workflow(
            LlmRole {
                client: put_client.clone(),
                model: "legacy-role-model".into(),
                thinking_level: None,
            },
            LlmRole {
                client: Arc::new(MockLlmClient::scripted(vec![ChatResponse {
                    content: Some(r#"{"item":"sample"}"#.into()),
                    thinking: None,
                    tool_calls: vec![],
                    usage: None,
                }])),
                model: "sim-model".into(),
                thinking_level: None,
            },
            RunnerOptions::default(),
            investigation().budget,
            put(),
            runtime(),
            None,
            Arc::new(Mutex::new(RunProgress::default())),
            WorkflowProgram {
                lua_source: crate::model::workflow::DEFAULT_WORKFLOW_LUA.into(),
                params: json!({"prompt":"custom prompt","model":"mock::chosen"}),
                ..Default::default()
            },
            false,
        )
        .await
        .unwrap();
        assert_eq!(trace.workflow.unwrap().output, Some(json!("done")));
        let requests = put_client.requests.lock().unwrap();
        assert_eq!(requests[0].model, "mock::chosen");
        match &requests[0].messages[0] {
            Message::System { content } => assert_eq!(content, "custom prompt"),
            other => panic!("expected custom system prompt, got {other:?}"),
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn direct_tool_calls_share_state_and_are_recorded() {
        let workflow = WorkflowProgram {
            lua_source: r#"return function(params, ctx)
  local r = ctx.call_tool('store', {path='x', content='y'})
  return r
end"#
                .into(),
            params: Value::Null,
            ..Default::default()
        };
        let sim_client = Arc::new(MockLlmClient::scripted(vec![
            ChatResponse {
                content: Some(r#"{"item":"sample"}"#.into()),
                thinking: None,
                tool_calls: vec![],
                usage: None,
            },
            ChatResponse {
                content: Some(
                    r#"{"response":{"ok":true},"state_patch":{"files":{"x":"y"}}}"#.into(),
                ),
                thinking: None,
                tool_calls: vec![],
                usage: None,
            },
        ]));
        let trace = run_workflow(
            LlmRole {
                client: Arc::new(MockLlmClient::scripted(vec![])),
                model: "put-model".into(),
                thinking_level: None,
            },
            LlmRole {
                client: sim_client,
                model: "sim-model".into(),
                thinking_level: None,
            },
            RunnerOptions::default(),
            investigation().budget,
            put(),
            runtime(),
            None,
            Arc::new(Mutex::new(RunProgress::default())),
            workflow,
            false,
        )
        .await
        .unwrap();
        let wf = trace.workflow.unwrap();
        assert_eq!(wf.tool_calls.len(), 1);
        assert_eq!(wf.tool_calls[0].response.as_ref().unwrap()["ok"], true);
        assert_eq!(trace.final_world_state["files"]["x"], "y");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn hidden_tool_calls_are_blocked_when_tools_subset_is_empty() {
        let workflow = WorkflowProgram {
            lua_source: r#"return function(params, ctx)
  local r = ctx.run_agent({name='x', prompt='p', model='mock::m', input=ctx.input, tools={}})
  return r
end"#
                .into(),
            ..Default::default()
        };
        let put_client = Arc::new(MockLlmClient::scripted(vec![
            ChatResponse {
                content: None,
                thinking: None,
                tool_calls: vec![crate::llm::ToolCallRequest {
                    id: "call-1".into(),
                    name: "store".into(),
                    arguments: r#"{"path":"x","content":"y"}"#.into(),
                }],
                usage: None,
            },
            ChatResponse {
                content: Some("done".into()),
                thinking: None,
                tool_calls: vec![],
                usage: None,
            },
        ]));
        let trace = run_workflow(
            LlmRole {
                client: put_client,
                model: "put-model".into(),
                thinking_level: None,
            },
            LlmRole {
                client: Arc::new(MockLlmClient::scripted(vec![ChatResponse {
                    content: Some(r#"{"item":"sample"}"#.into()),
                    thinking: None,
                    tool_calls: vec![],
                    usage: None,
                }])),
                model: "sim-model".into(),
                thinking_level: None,
            },
            RunnerOptions::default(),
            investigation().budget,
            put(),
            runtime(),
            None,
            Arc::new(Mutex::new(RunProgress::default())),
            workflow,
            false,
        )
        .await
        .unwrap();
        let exchange = &trace.turns[0].tool_exchanges[0];
        assert!(
            exchange
                .response
                .as_str()
                .unwrap()
                .contains("was not exposed")
        );
        assert!(trace.final_world_state.is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn local_token_cutoffs_are_retained_per_invocation_and_output_is_nil() {
        let workflow = WorkflowProgram {
            lua_source: r#"return function(params, ctx)
  local a = ctx.run_agent({name='a', prompt='p', model='mock::m', input=ctx.input, budget={max_tokens=1}})
  local b = ctx.run_agent({name='b', prompt='p', model='mock::m', input=ctx.input, budget={max_tokens=1}})
  return {
    a_output_nil = a.output == nil,
    b_output_nil = b.output == nil,
    a_cut = a.budget_cutoff_completion.model_output,
    b_cut = b.budget_cutoff_completion.model_output,
  }
end"#
                .into(),
            ..Default::default()
        };
        let put_client = Arc::new(MockLlmClient::scripted(vec![
            ChatResponse {
                content: Some("first-cutoff".into()),
                thinking: Some("t1".into()),
                tool_calls: vec![],
                usage: Some(crate::llm::Usage {
                    input_tokens: 1,
                    cache_read_tokens: 0,
                    output_tokens: 1,
                }),
            },
            ChatResponse {
                content: Some("second-cutoff".into()),
                thinking: Some("t2".into()),
                tool_calls: vec![],
                usage: Some(crate::llm::Usage {
                    input_tokens: 1,
                    cache_read_tokens: 0,
                    output_tokens: 1,
                }),
            },
        ]));
        let trace = run_workflow(
            LlmRole {
                client: put_client,
                model: "put-model".into(),
                thinking_level: None,
            },
            LlmRole {
                client: Arc::new(MockLlmClient::scripted(vec![ChatResponse {
                    content: Some(r#"{"item":"sample"}"#.into()),
                    thinking: None,
                    tool_calls: vec![],
                    usage: None,
                }])),
                model: "sim-model".into(),
                thinking_level: None,
            },
            RunnerOptions::default(),
            investigation().budget,
            put(),
            runtime(),
            None,
            Arc::new(Mutex::new(RunProgress::default())),
            workflow,
            false,
        )
        .await
        .unwrap();
        let wf = trace.workflow.unwrap();
        assert_eq!(
            wf.output,
            Some(json!({
                "a_output_nil": true,
                "b_output_nil": true,
                "a_cut": "first-cutoff",
                "b_cut": "second-cutoff"
            }))
        );
        assert_eq!(wf.invocations.len(), 2);
        assert_eq!(
            wf.invocations[0].stop_reason,
            Some(RunStopReason::TokenBudget)
        );
        assert_eq!(
            wf.invocations[1].stop_reason,
            Some(RunStopReason::TokenBudget)
        );
        assert_eq!(
            wf.invocations[0]
                .budget_cutoff_completion
                .as_ref()
                .unwrap()
                .model_output
                .as_deref(),
            Some("first-cutoff")
        );
        assert_eq!(
            wf.invocations[1]
                .budget_cutoff_completion
                .as_ref()
                .unwrap()
                .model_output
                .as_deref(),
            Some("second-cutoff")
        );
        assert_eq!(wf.invocations[0].output, None);
        assert_eq!(wf.invocations[1].output, None);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn run_agent_failure_is_structured_and_keeps_partial_evidence() {
        let workflow = WorkflowProgram {
            lua_source: r#"return function(params, ctx)
  local r = ctx.run_agent({name='x', prompt='p', model='mock::m', input=ctx.input})
  if r.failure ~= nil then return {kind='failed', error=r.failure.error, invocation_id=r.failure.invocation_id} end
  return r.output
end"#
                .into(),
            ..Default::default()
        };
        let err = run_workflow(
            LlmRole {
                client: Arc::new(MockLlmClient::scripted(vec![])),
                model: "put-model".into(),
                thinking_level: None,
            },
            LlmRole {
                client: Arc::new(MockLlmClient::scripted(vec![ChatResponse {
                    content: Some(r#"{"item":"sample"}"#.into()),
                    thinking: None,
                    tool_calls: vec![],
                    usage: None,
                }])),
                model: "sim-model".into(),
                thinking_level: None,
            },
            RunnerOptions::default(),
            investigation().budget,
            put(),
            runtime(),
            None,
            Arc::new(Mutex::new(RunProgress::default())),
            workflow,
            false,
        )
        .await
        .unwrap();
        assert_eq!(
            err.workflow.unwrap().output,
            Some(
                json!({"kind":"failed","error":"PUT model call failed: provider request failed: mock script exhausted","invocation_id":0})
            )
        );
    }
}
