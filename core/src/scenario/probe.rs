//! Simulation probes: caller-submitted tool calls executed against a scenario
//! through the SAME engine an investigation uses.
//!
//! This is how a driving agent iterates on a simulation without a local Lua
//! toolchain and without spending investigations: it uploads a workspace once,
//! posts calls, reads real responses (including Lua provenance and rollbacks),
//! edits the scenario, and repeats. Probes never invoke the prompt under test
//! and never become frontier candidates.
//!
//! Semantics: each submission starts from a fresh snapshot of the scenario's
//! workspace and a fresh simulator conversation; calls within one submission run
//! sequentially in that one session, so writes from call 1 are visible to call 2.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::llm::{LlmClient, LlmError, ThinkingLevel};
use crate::model::simulation::{LuaExecutionRecord, ToolCall, WorkspaceOp};

use super::store::ScenarioRecord;
use crate::simulate::engine::{ScenarioRuntime, SimEngine};

/// Hard ceiling on tool calls in one probe submission: a probe is a development
/// loop, not a batch runner, and every call may cost a simulator completion.
pub const MAX_PROBE_CALLS: usize = 200;

/// What a caller submits to test a simulation.
#[derive(Debug, Clone, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ProbeRequest {
    /// The calls to render, in order. Each runs in the scenario's language:
    /// `{"name": "<tool>", "args": {...}}`. Arguments are validated against the
    /// declared tool schema exactly as a PUT's call would be.
    pub tool_calls: Vec<ToolCall>,
    /// Pin the scenario's declared inputs for this probe instead of sampling
    /// them. Supply every declared key; omit to sample.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_inputs: Option<HashMap<String, Value>>,
    /// Optional guard: refuse to probe a scenario that has moved on since the
    /// caller last read it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_revision: Option<u64>,
    /// Optional lower cap on how many of `tool_calls` to run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_calls: Option<usize>,
    /// Free-form note about what this probe is checking (surfaced with the
    /// result; never interpreted).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl ProbeRequest {
    /// Validate what deterministic code can check before spending anything.
    pub fn validate(&self) -> Result<(), String> {
        if self.tool_calls.is_empty() {
            return Err("tool_calls must contain at least one call".into());
        }
        let cap = self.max_calls.unwrap_or(self.tool_calls.len());
        if cap == 0 {
            return Err("max_calls must be at least 1".into());
        }
        if cap > MAX_PROBE_CALLS {
            return Err(format!(
                "max_calls is {cap}, above the {MAX_PROBE_CALLS}-call probe ceiling"
            ));
        }
        if self.tool_calls.len() > MAX_PROBE_CALLS {
            return Err(format!(
                "{} tool calls, above the {MAX_PROBE_CALLS}-call probe ceiling",
                self.tool_calls.len()
            ));
        }
        for call in &self.tool_calls {
            if call.name.trim().is_empty() {
                return Err("tool call names must not be empty".into());
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProbeStatus {
    Running,
    Done,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProbeStopReason {
    /// Every requested call was rendered.
    Completed,
    /// `max_calls` (or the ceiling) stopped the sequence.
    CallLimit,
    /// A call could not be rendered; earlier calls remain as evidence.
    RuntimeFailure,
}

/// One rendered call, with the provenance needed to judge it.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ProbeCall {
    /// The submitted request, echoed.
    pub request: ToolCall,
    /// The rendered tool response. `null` when this call failed and none was
    /// produced; `error` then says why.
    #[schema(required = true)]
    pub response: Option<Value>,
    /// Present for write tools: world state after this call's patch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state_after: Option<HashMap<String, Value>>,
    /// Present when a supplied Lua implementation was attempted for this call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lua_execution: Option<LuaExecutionRecord>,
    /// Workspace operations the simulator performed (its lookups, plus any
    /// committed Lua writes). Rolled-back Lua writes appear under
    /// `lua_execution.discarded_workspace_ops` instead.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub workspace_ops: Vec<WorkspaceOp>,
    /// The simulator model's visible reasoning while rendering this response.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sim_thinking: Option<String>,
    pub elapsed_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Live and terminal state of one probe. `status` is running until the sequence
/// stops; a failed sequence keeps every completed call.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ProbeProgress {
    pub status: ProbeStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<ProbeStopReason>,
    pub started_at: u64,
    #[schema(required = true)]
    pub finished_at: Option<u64>,
    /// The inputs this probe actually ran with (sampled or caller-supplied).
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub resolved_inputs: HashMap<String, Value>,
    pub calls: Vec<ProbeCall>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl ProbeProgress {
    pub fn new(started_at: u64) -> Self {
        Self {
            status: ProbeStatus::Running,
            stop_reason: None,
            started_at,
            finished_at: None,
            resolved_inputs: HashMap::new(),
            calls: Vec::new(),
            error: None,
        }
    }

    pub fn finish(&mut self, stop_reason: ProbeStopReason, finished_at: u64) {
        self.stop_reason = Some(stop_reason);
        self.status = match stop_reason {
            ProbeStopReason::RuntimeFailure => ProbeStatus::Failed,
            _ => ProbeStatus::Done,
        };
        self.finished_at = Some(finished_at);
    }
}

/// Everything the probe needs from the scenario record: the definition, its
/// runtime projection, and the identity to report.
pub struct ProbeTarget {
    pub scenario_id: String,
    pub revision: u64,
    pub definition_hash: String,
    pub runtime: ScenarioRuntime,
}

impl ProbeTarget {
    /// Build a probe target from a stored record.
    pub fn from_record(record: &ScenarioRecord, workspace: crate::simulate::Workspace) -> Self {
        let runtime = runtime_for(record, workspace);
        Self {
            scenario_id: record.id.clone(),
            revision: record.revision,
            definition_hash: record.definition_hash.clone(),
            runtime,
        }
    }
}

/// The runtime projection of a stored scenario: narrative, contracts,
/// implementations, simulator settings and a private workspace snapshot.
pub fn runtime_for(
    record: &ScenarioRecord,
    workspace: crate::simulate::Workspace,
) -> ScenarioRuntime {
    ScenarioRuntime::from_definition(&record.definition, workspace)
}

/// Execute one probe sequence, updating `progress` as it goes so a caller can
/// watch it and read partial evidence after a failure.
#[allow(clippy::too_many_arguments)]
pub async fn run_probe(
    client: Arc<dyn LlmClient>,
    model: impl Into<String>,
    thinking_level: Option<ThinkingLevel>,
    target: &ProbeTarget,
    request: &ProbeRequest,
    progress: Arc<Mutex<ProbeProgress>>,
    now: impl Fn() -> u64,
) {
    let mut engine = match SimEngine::start(
        client,
        model,
        thinking_level,
        &target.runtime,
        request.resolved_inputs.as_ref(),
        None,
    )
    .await
    {
        Ok(engine) => engine,
        Err(error) => {
            if let Ok(mut progress) = progress.lock() {
                progress.error = Some(error.to_string());
                progress.finish(ProbeStopReason::RuntimeFailure, now());
            }
            return;
        }
    };
    if let Ok(mut progress) = progress.lock() {
        progress.resolved_inputs = engine.resolved_inputs().clone();
    }
    let cap = request
        .max_calls
        .unwrap_or(request.tool_calls.len())
        .min(MAX_PROBE_CALLS);
    let requested = request.tool_calls.len();
    let mut stop = ProbeStopReason::Completed;
    for (index, call) in request.tool_calls.iter().take(cap).enumerate() {
        let started = now();
        let outcome = engine.call_value(&call.name, call.args.clone()).await;
        let elapsed_ms = now().saturating_sub(started);
        let mut record = ProbeCall {
            request: call.clone(),
            response: None,
            state_after: None,
            lua_execution: None,
            workspace_ops: Vec::new(),
            sim_thinking: None,
            elapsed_ms,
            error: None,
        };
        match outcome {
            Ok(rendered) => {
                record.response = Some(rendered.response);
                record.state_after = rendered.state_after;
                record.lua_execution = rendered.lua_execution;
                record.workspace_ops = rendered.workspace_ops;
                record.sim_thinking = rendered.sim_thinking;
            }
            Err(error) => {
                record.error = Some(error.to_string());
                stop = ProbeStopReason::RuntimeFailure;
                if let Ok(mut progress) = progress.lock() {
                    progress.error = Some(format!(
                        "call {}/{} ('{}') could not be rendered: {}",
                        index + 1,
                        requested,
                        call.name,
                        error
                    ));
                    progress.calls.push(record);
                    progress.finish(stop, now());
                }
                return;
            }
        }
        if let Ok(mut progress) = progress.lock() {
            progress.calls.push(record);
        }
    }
    if cap < requested {
        stop = ProbeStopReason::CallLimit;
    }
    if let Ok(mut progress) = progress.lock() {
        progress.finish(stop, now());
    }
}

/// Wrap a `LlmError` as a probe failure for callers that only need a message.
pub fn probe_failure(error: &LlmError) -> String {
    error.to_string()
}