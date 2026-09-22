//! Lua investigation orchestration: caller-authored workflow source, opaque
//! params, resource limits, and the evidence emitted by execution.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::ToSchema;

use crate::llm::ThinkingLevel;
use crate::model::lua::{
    MAX_DURATION_MS, MAX_HOST_BYTES, MAX_HOST_CALLS, MAX_INSTRUCTIONS, MAX_MEMORY_BYTES,
    MAX_RESULT_BYTES, MAX_SOURCE_BYTES, MAX_VALUE_DEPTH,
};
use crate::model::simulation::{LuaExecutionRecord, RunStopReason, WorkspaceOp};

/// The built-in single-stage program. There is nothing privileged about it:
/// it is ordinary workflow Lua that happens to read `params.prompt`,
/// `params.model` and `params.controls`. `Investigator::investigate` uses it
/// as the library shorthand for a one-agent run. `ctx.render` fills the
/// scenario's sampled `{{input_domain}}` values into the prompt text.
pub const DEFAULT_WORKFLOW_LUA: &str = r#"return function(params, ctx)
  local agent = ctx.run_agent({
    name = "put",
    prompt = ctx.render(params.prompt),
    model = params.model,
    controls = params.controls,
    input = ctx.input,
  })
  if agent.failure ~= nil then
    error(agent.failure.error)
  end
  return agent.output
end"#;

/// HARD APPLICATION-ORCHESTRATION limits, NOT scenario tool-handler limits.
/// Exceeding these stops/fails the workflow and retains partial evidence. It
/// NEVER asks the simulator LLM to execute, repair or continue the program.
/// ctx.run_agent never delegates to the simulator either. A scenario tool's
/// own Lua handler may separately fall back to simulation; that distinct
/// policy is governed by the scenario's LuaOptions, not these limits.
/// Direct calls are recorded in workflow.tool_calls; agent conversations in
/// workflow.invocations plus their ranges into the flat turns array.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(default, deny_unknown_fields)]
pub struct WorkflowLimits {
    /// Max ctx.run_agent invocations.
    #[schema(minimum = 1)]
    pub max_agent_invocations: usize,
    /// Max direct ctx.call_tool invocations.
    #[schema(minimum = 1)]
    pub max_direct_tool_calls: usize,
    /// Lua allocator limit.
    #[schema(minimum = 1, maximum = 67108864)]
    pub max_memory_bytes: usize,
    /// Shared VM/conversion instruction budget.
    #[schema(minimum = 1, maximum = 10000000)]
    pub max_instructions: u64,
    /// Combined ctx.run_agent/ctx.call_tool call limit.
    #[schema(minimum = 1, maximum = 1024)]
    pub max_host_calls: usize,
    /// Combined serialized argument/result bytes over ctx.run_agent/ctx.call_tool.
    #[schema(minimum = 1, maximum = 33554432)]
    pub max_host_bytes: usize,
    /// UTF-8 source length limit before parsing.
    #[schema(minimum = 1, maximum = 1048576)]
    pub max_source_bytes: usize,
    /// Cooperative wall-clock deadline for Lua CPU time only.
    #[schema(minimum = 1, maximum = 10000)]
    pub max_duration_ms: u64,
    /// Maximum JSON/Lua nesting depth.
    #[schema(minimum = 1, maximum = 128)]
    pub max_value_depth: usize,
    /// Maximum serialized size of one converted value / final workflow output.
    #[schema(minimum = 1, maximum = 4194304)]
    pub max_result_bytes: usize,
}

impl Default for WorkflowLimits {
    fn default() -> Self {
        Self {
            max_agent_invocations: 16,
            max_direct_tool_calls: 256,
            max_memory_bytes: 16 * 1024 * 1024,
            max_instructions: 1_000_000,
            max_host_calls: 128,
            max_host_bytes: 8 * 1024 * 1024,
            max_source_bytes: 256 * 1024,
            max_duration_ms: 2_000,
            max_value_depth: MAX_VALUE_DEPTH,
            max_result_bytes: 1024 * 1024,
        }
    }
}

/// The APPLICATION under test, owned by the investigation, not the scenario.
/// To test extract -> review, make TWO ctx.run_agent calls in ONE program;
/// do not stitch independent investigations together. Params has no privileged
/// keys. The default program is merely a convention using prompt/model/controls.
///
/// This is NOT scenario tool-handler Lua. Its ctx has render, run_agent,
/// call_tool, input and resolved_inputs; NO ctx.workspace, world state,
/// simulator notes, or tool implementation source. To read a file, invoke the scenario tool:
/// ctx.call_tool("read_file", {path="config.json"}), not ctx.workspace.read.
/// Read GET /docs/workflow for executable examples and the full contract.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(default, deny_unknown_fields)]
pub struct WorkflowProgram {
    /// Lua chunk returning function(params, ctx). Omit to run the default
    /// program with ordinary params.prompt, params.model and optional
    /// params.controls. Custom programs can loop, branch and transform data.
    /// ctx.render(text) validates and fills `{{input_domain}}` values.
    /// ctx.run_agent{prompt=...,model=...,input=...,tools={...},controls={...}}
    /// returns ONE result table: output is nil without a final completion;
    /// failure describes an ordinary agent error; stop_reason, invocation_id,
    /// steps_used and tokens_used retain execution facts. Use qualified model
    /// identifiers from /api/models. Each call gets a fresh conversation;
    /// scenario world/workspace and total budgets are shared across calls.
    /// Exact input handoffs are recorded, not re-simulated. ctx.call_tool(name,
    /// args) returns that scenario tool's response using the same simulation
    /// engine. Workflow errors NEVER delegate orchestration to the simulator.
    pub lua_source: String,
    /// Arbitrary caller-owned JSON. The harness assigns it no semantics.
    pub params: Value,
    /// Sandbox and orchestration resource limits.
    pub limits: WorkflowLimits,
}

impl Default for WorkflowProgram {
    fn default() -> Self {
        Self {
            lua_source: DEFAULT_WORKFLOW_LUA.into(),
            params: Value::Null,
            limits: WorkflowLimits::default(),
        }
    }
}

impl WorkflowProgram {
    pub fn default_program() -> Self {
        Self::default()
    }
}

/// Exact effective agent controls for one ctx.run_agent call.
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
#[serde(default, deny_unknown_fields)]
pub struct AgentControls {
    pub thinking: Option<ThinkingLevel>,
    pub temperature: Option<f32>,
    #[schema(minimum = 1)]
    pub max_tokens: Option<u32>,
}

/// Evidence for one ctx.run_agent stage.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AgentInvocationRecord {
    /// One shared, monotonic event sequence across agent invocations AND direct
    /// ctx.call_tool events.
    pub event_id: u64,
    /// Stable per-invocation id, returned to Lua so the workflow can branch or
    /// retry with explicit references.
    pub invocation_id: u64,
    pub name: String,
    pub prompt: String,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<String>,
    #[serde(default)]
    pub tools: Vec<String>,
    #[serde(default)]
    pub controls: AgentControls,
    /// Effective remaining step/token caps at this invocation's start, limited
    /// by both the requested local caps and the investigation's remaining budget.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget: Option<crate::model::input::Budget>,
    pub turn_start: usize,
    pub turn_end: usize,
    pub steps_used: u64,
    pub tokens_used: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<RunStopReason>,
    #[serde(default)]
    pub running: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget_cutoff_completion: Option<crate::model::simulation::BudgetCutoffCompletion>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unrendered_call: Option<crate::model::simulation::ToolCall>,
}

/// Evidence for one direct ctx.call_tool call.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DirectToolCallRecord {
    /// One shared, monotonic event sequence across agent invocations AND direct
    /// ctx.call_tool events.
    pub event_id: u64,
    pub name: String,
    pub args: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state_after: Option<HashMap<String, Value>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub workspace_ops: Vec<WorkspaceOp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lua_execution: Option<LuaExecutionRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sim_thinking: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<String>,
    #[serde(default)]
    pub running: bool,
}

/// Complete orchestration evidence, retained on the finished trace and in live
/// progress while the workflow is still running or has failed.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct WorkflowEvidence {
    pub source: String,
    pub source_hash: String,
    pub params: Value,
    /// Effective orchestration limits, including for the default program.
    /// Absent only in older evidence recorded before these limits were surfaced.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limits: Option<WorkflowLimits>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub invocations: Vec<AgentInvocationRecord>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<DirectToolCallRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<RunStopReason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl WorkflowLimits {
    pub fn validate(&self) -> Result<(), String> {
        for (name, value, maximum) in [
            (
                "max_memory_bytes",
                self.max_memory_bytes as u64,
                MAX_MEMORY_BYTES as u64,
            ),
            ("max_instructions", self.max_instructions, MAX_INSTRUCTIONS),
            (
                "max_host_calls",
                self.max_host_calls as u64,
                MAX_HOST_CALLS as u64,
            ),
            (
                "max_host_bytes",
                self.max_host_bytes as u64,
                MAX_HOST_BYTES as u64,
            ),
            (
                "max_source_bytes",
                self.max_source_bytes as u64,
                MAX_SOURCE_BYTES as u64,
            ),
            ("max_duration_ms", self.max_duration_ms, MAX_DURATION_MS),
            (
                "max_value_depth",
                self.max_value_depth as u64,
                MAX_VALUE_DEPTH as u64,
            ),
            (
                "max_result_bytes",
                self.max_result_bytes as u64,
                MAX_RESULT_BYTES as u64,
            ),
        ] {
            if value == 0 {
                return Err(format!("workflow limit '{name}' must be greater than zero"));
            }
            if value > maximum {
                return Err(format!("workflow limit '{name}' must not exceed {maximum}"));
            }
        }
        for (name, value) in [
            ("max_agent_invocations", self.max_agent_invocations),
            ("max_direct_tool_calls", self.max_direct_tool_calls),
        ] {
            if value == 0 {
                return Err(format!("workflow limit '{name}' must be greater than zero"));
            }
        }
        Ok(())
    }
}
