//! A reusable scenario definition: the authored world, its tool contracts,
//! caller-supplied tool implementations, and the simulator settings that
//! belong to the scenario rather than to any one investigation.
//!
//! Splitting the definition from the run is what lets a caller upload a
//! workspace once, develop and test a simulation before spending an
//! investigation, and pin the exact revision that produced a trace. The
//! narrative remains ground truth; Lua source is an optional implementation
//! artifact the caller authors — the harness never generates or rewrites it.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::llm::ThinkingLevel;
use crate::model::Scenario;
use crate::model::input::{SideEffect, ToolSchema};
use crate::model::lua::LuaOptions;

/// One tool the scenario's world exposes: the contract the prompt under test
/// sees, plus an OPTIONAL caller-authored Lua implementation of it.
///
/// The contract is model-visible; `lua_source` never is. A handler that cannot
/// faithfully implement the declared semantics should call
/// `PleaseSimulateException("reason")` and let the simulator LLM render that
/// one response instead.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ScenarioTool {
    pub name: String,
    /// The tool contract, exactly as the prompt under test sees it and as the
    /// simulator is told to render it. Describe argument semantics AND the
    /// returned shape. A vague contract makes Lua and LLM implementations
    /// disagree silently.
    pub description: String,
    /// JSON Schema for the tool's parameters.
    pub parameters: Value,
    pub side_effect: SideEffect,
    /// Realism hints for the simulator LLM (anchors, not pinned outputs).
    #[serde(default)]
    pub example_responses: Vec<String>,
    /// Optional Lua implementation, run in the sandbox for every call of this
    /// tool. The source RETURNS a function taking `(args, ctx)`:
    ///
    /// ```lua
    /// return function(args, ctx)
    ///   return { response = { ... }, state_patch = { ... } } -- writes only
    /// end
    /// ```
    ///
    /// Omit it to let the simulator LLM render this tool's responses. A
    /// handler may decline a call with `PleaseSimulateException("reason")`;
    /// missing handlers and runtime errors also delegate, and their staged
    /// workspace writes are discarded.
    ///
    /// `ctx` exposes `ctx.workspace` — `list_dir`, `read`, `grep` and `write`
    /// over the run's private copy of the scenario's uploaded workspace — so a
    /// handler can serve EXACT file contents instead of asking a model to
    /// reproduce them (see `LuaWorkspaceCapability` for the per-operation
    /// shapes). The full prose reference, including the sandbox limits and how
    /// computed/delegated/errored calls are recorded, is served by the running
    /// server at `GET /docs/lua`. Read
    /// `execution.lua_computed_calls`/`lua_fallback_calls`/`lua_error_calls`
    /// after a run to check that your code actually served it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lua_source: Option<String>,
}

impl ScenarioTool {
    /// The model-visible contract, without simulator-private implementation.
    pub fn contract(&self) -> ToolSchema {
        ToolSchema {
            name: self.name.clone(),
            description: self.description.clone(),
            parameters: self.parameters.clone(),
            side_effect: self.side_effect,
            example_responses: self.example_responses.clone(),
        }
    }
}

/// One caller-authored Lua implementation, as evidence: the tool it serves,
/// its source, and a hash identifying the exact revision that executed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ToolImplementation {
    pub tool: String,
    pub source: String,
    pub source_hash: String,
}

impl ToolImplementation {
    pub fn new(tool: impl Into<String>, source: impl Into<String>) -> Self {
        let source = source.into();
        let source_hash = source_hash(&source);
        Self {
            tool: tool.into(),
            source,
            source_hash,
        }
    }
}

/// SHA-256 of the exact bytes a caller supplied, so a trace names the
/// implementation revision that ran.
pub fn source_hash(source: &str) -> String {
    let mut hash = Sha256::new();
    hash.update(source.as_bytes());
    format!("{:x}", hash.finalize())
}

/// Per-tool workspace capability bounds. `None` keeps the documented default.
#[derive(Debug, Clone, Default, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(default, deny_unknown_fields)]
pub struct WorkspaceLimits {
    pub max_read_lines: Option<usize>,
    pub max_grep_matches: Option<usize>,
    pub max_line_len: Option<usize>,
    #[schema(maximum = 4194304)]
    pub max_output_bytes: Option<usize>,
}

/// Simulator configuration that belongs to the SCENARIO, not to an
/// investigation: the environment is part of the test case, and testing a
/// simulation under different settings than an investigation runs it under
/// should be an explicit edit (or a fork), never an invisible override.
#[derive(Debug, Clone, Default, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(default, deny_unknown_fields)]
pub struct SimulationSettings {
    /// Model that roleplays the environment. Omit for the server default.
    pub sim_model: Option<String>,
    /// Reasoning effort for simulator completions. Omit for the provider default.
    pub sim_thinking_level: Option<ThinkingLevel>,
    /// Simulator sampling temperature. Omit for the documented default.
    pub temperature: Option<f32>,
    /// Maximum output tokens per simulator completion.
    #[schema(minimum = 1)]
    pub max_tokens: Option<u32>,
    /// Total attempts per simulator JSON reply (initial reply included).
    #[schema(minimum = 1)]
    pub max_repair_attempts: Option<usize>,
    /// Workspace tool calls per simulator response before the final-answer nudge.
    #[schema(minimum = 0)]
    pub max_workspace_turns: Option<usize>,
    /// Resource limits for caller-supplied Lua handlers. Omit for defaults.
    pub lua: Option<LuaOptions>,
    /// Bounds on simulator workspace tool output.
    pub workspace: WorkspaceLimits,
}

/// A reusable test case: the authored world (narrative, input domain,
/// protagonist, simulator notes), the tool surface with its optional Lua
/// implementations, and the simulator settings to run it under.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ScenarioDefinition {
    /// The world specification — ground truth the simulator renders tool
    /// responses from, and that the caller checks claims against.
    ///
    /// Cover inventory, facts (including NEGATIVE facts), completeness
    /// assertions, and rendering rules. Embed authoritative documentation
    /// (an OpenAPI spec, a man page) verbatim and pin rendering to it.
    pub world: String,
    /// Per-`{{variable}}` input-domain descriptions: the value space,
    /// semantics, and preconditions/trust contracts. The simulator picks a
    /// concrete value from each domain and the chosen value is reported in
    /// `resolved_inputs`; `POST .../simulations` can also supply explicit
    /// bindings to test one selection directly.
    #[serde(default)]
    pub input_domain: HashMap<String, String>,
    /// The opening message from the user/protagonist.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_message: Option<String>,
    /// Persona/stance guidance for a simulated user, if the scenario has one.
    #[serde(default)]
    pub simulator_notes: String,
    /// The tool surface: contracts plus optional Lua implementations.
    #[serde(default)]
    pub tools: Vec<ScenarioTool>,
    /// Simulator settings this scenario runs under.
    #[serde(default)]
    pub simulation: SimulationSettings,
}

impl ScenarioDefinition {
    /// The authored narrative, as the evidence/trace model reports it.
    pub fn scenario(&self) -> Scenario {
        Scenario {
            world: self.world.clone(),
            input_domain: self.input_domain.clone(),
            user_message: self.user_message.clone(),
            simulator_notes: self.simulator_notes.clone(),
        }
    }

    /// The model-visible tool contracts of this scenario.
    pub fn tool_contracts(&self) -> Vec<ToolSchema> {
        self.tools.iter().map(ScenarioTool::contract).collect()
    }

    /// The supplied Lua implementations, in tool order.
    pub fn implementations(&self) -> Vec<ToolImplementation> {
        self.tools
            .iter()
            .filter_map(|tool| {
                tool.lua_source
                    .as_ref()
                    .map(|source| ToolImplementation::new(tool.name.clone(), source.clone()))
            })
            .collect()
    }

    /// SHA-256 over everything that can change an execution: the narrative,
    /// the domains, the tool contracts, the implementation sources, the
    /// simulator settings, and the workspace (hashed separately by the store).
    /// Display-only metadata is deliberately excluded.
    pub fn content_hash(&self) -> String {
        let mut hash = Sha256::new();
        hash_update(&mut hash, &self.world);
        let mut domains: Vec<_> = self.input_domain.iter().collect();
        domains.sort_by(|(a, _), (b, _)| a.cmp(b));
        for (name, domain) in domains {
            hash_update(&mut hash, name);
            hash_update(&mut hash, domain);
        }
        hash_update(&mut hash, self.user_message.as_deref().unwrap_or(""));
        hash_update(&mut hash, &self.simulator_notes);
        for tool in &self.tools {
            hash_update(&mut hash, &tool.name);
            hash_update(&mut hash, &tool.description);
            hash_update(
                &mut hash,
                &serde_json::to_string(&tool.parameters).unwrap_or_default(),
            );
            hash_update(
                &mut hash,
                match tool.side_effect {
                    SideEffect::Read => "read",
                    SideEffect::Write => "write",
                },
            );
            for example in &tool.example_responses {
                hash_update(&mut hash, example);
            }
            hash_update(&mut hash, tool.lua_source.as_deref().unwrap_or(""));
        }
        hash_update(
            &mut hash,
            &serde_json::to_string(&self.simulation).unwrap_or_default(),
        );
        format!("{:x}", hash.finalize())
    }

    /// Validate what deterministic code can check before anything runs: tool
    /// names, and that supplied Lua source parses as a module returning a
    /// function. This is syntax/loading validation only — it does not execute
    /// the handler and is not a statement about fidelity.
    pub fn validate(&self) -> Result<(), String> {
        let mut seen = std::collections::BTreeSet::new();
        for tool in &self.tools {
            if tool.name.trim().is_empty() {
                return Err("tool names must not be empty".into());
            }
            if !seen.insert(tool.name.clone()) {
                return Err(format!("duplicate tool name '{}'", tool.name));
            }
            if let Some(source) = &tool.lua_source {
                let limits = self.simulation.lua.clone().unwrap_or_default();
                crate::simulate::lua::check_source(&tool.name, source, &limits)
                    .map_err(|error| format!("tool '{}': {error}", tool.name))?;
            }
        }
        for (name, domain) in &self.input_domain {
            if name.trim().is_empty() {
                return Err("input_domain keys must not be empty".into());
            }
            if domain.trim().is_empty() {
                return Err(format!("input_domain '{name}' has an empty description"));
            }
        }
        if let Some(lua) = &self.simulation.lua {
            lua.validate()
                .map_err(|error| format!("simulation.lua: {error}"))?;
        }
        Ok(())
    }
}

fn hash_update(hash: &mut Sha256, value: &str) {
    hash.update((value.len() as u64).to_le_bytes());
    hash.update(value.as_bytes());
}

/// Where a corrected scenario came from. Purely descriptive history: it does
/// NOT lock, validate, or invalidate anything, and deleting the predecessor
/// does not break the link.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct Correction {
    /// The scenario this one corrects or replaces.
    pub scenario_id: String,
    /// That scenario's revision when this fork was made.
    pub revision: u64,
    /// Caller-written explanation of what changed and why.
    pub reason: String,
}
