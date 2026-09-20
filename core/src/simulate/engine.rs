//! The ONE tool-call execution path, shared by investigations and probes.
//!
//! An investigation drives this engine with calls a model chose; a probe drives
//! it with calls a caller submitted. Everything else is identical: the same
//! argument validation, the same supplied-Lua execution and sandbox limits, the
//! same delegation to the simulator LLM when the implementation declines or
//! crashes, the same workspace overlay and rollback, and the same world-state
//! patches. A probe is therefore evidence about what an investigation will do,
//! not evidence about a second, differently-behaving simulator.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde_json::{Map, Value};

use crate::llm::{LlmClient, LlmError, ThinkingLevel};
use crate::model::ToolSchema;
use crate::model::scenario::ToolImplementation;
use crate::model::simulation::{
    LuaExecutionRecord, RunProgress, RunStopReason, ToolCall, WorkspaceOp,
};
use crate::model::{Scenario, SideEffect};

use super::simulator::{SimSession, SimulatorOptions, ToolSimulator, apply_patch};
use super::workspace::Workspace;

/// Everything a run needs from a scenario: the narrative, the tool surface, the
/// caller-supplied implementations, and the immutable workspace seed. A probe
/// and an investigation both build this from the same stored scenario record.
#[derive(Clone)]
pub struct ScenarioRuntime {
    pub scenario: Scenario,
    pub tools: Vec<ToolSchema>,
    pub implementations: Vec<ToolImplementation>,
    pub workspace_seed: Workspace,
    /// Simulator limits and sampling. These belong to the scenario (the
    /// environment is part of the test case), so they travel with the runtime
    /// instead of being re-supplied per investigation.
    pub simulator: SimulatorOptions,
    /// The scenario's declared simulator settings, kept alongside the resolved
    /// options so a reader (and the job view) can report exactly what was asked
    /// for: model, thinking level, and every override.
    pub settings: crate::model::scenario::SimulationSettings,
}

impl ScenarioRuntime {
    pub fn new(
        scenario: Scenario,
        tools: Vec<ToolSchema>,
        implementations: Vec<ToolImplementation>,
        workspace_seed: Workspace,
        simulator: SimulatorOptions,
    ) -> Self {
        Self {
            scenario,
            tools,
            implementations,
            workspace_seed,
            simulator,
            settings: Default::default(),
        }
    }

    /// The runtime projection of a scenario definition: its narrative, tool
    /// contracts, supplied implementations, and resolved simulator settings.
    pub fn from_definition(
        definition: &crate::model::scenario::ScenarioDefinition,
        workspace_seed: Workspace,
    ) -> Self {
        let implementations = definition.implementations();
        let simulator =
            SimulatorOptions::from_settings(&definition.simulation, !implementations.is_empty());
        Self {
            scenario: definition.scenario(),
            tools: definition.tool_contracts(),
            implementations,
            workspace_seed,
            simulator,
            settings: definition.simulation.clone(),
        }
    }

    /// The common case: the tool contracts come from the prompt under test and
    /// no Lua implementation is supplied.
    pub fn from_put(
        put: &crate::model::PromptUnderTest,
        scenario: Scenario,
        workspace_seed: Workspace,
    ) -> Self {
        Self::new(
            scenario,
            put.tools.clone(),
            Vec::new(),
            workspace_seed,
            SimulatorOptions::default(),
        )
    }

    /// Whether any tool is served by caller-supplied Lua code. This is
    /// provenance ("how were responses rendered"), never a policy switch.
    pub fn lua_enabled(&self) -> bool {
        !self.implementations.is_empty()
    }
}

/// What one simulated tool call produced.
pub struct EngineCall {
    /// The value the prompt under test (or the probe caller) observes.
    pub response: Value,
    /// Present for write tools: world state after this call's patch.
    pub state_after: Option<HashMap<String, Value>>,
    /// Workspace operations the simulator performed for this call (its own
    /// lookups, plus any committed Lua writes).
    pub workspace_ops: Vec<WorkspaceOp>,
    /// The simulator model's visible reasoning, transparency only.
    pub sim_thinking: Option<String>,
    /// Present when a supplied implementation was attempted; names the tool and
    /// the exact source hash, and says whether it computed, delegated or errored.
    pub lua_execution: Option<LuaExecutionRecord>,
}

/// One scenario's simulation session: resolved inputs, mutable world state, an
/// isolated workspace, and the simulator conversation.
pub struct SimEngine {
    session: SimSession,
    tools: Vec<ToolSchema>,
    world_state: Map<String, Value>,
    resolved_inputs: HashMap<String, Value>,
}

impl SimEngine {
    /// Start a session, resolving the scenario's declared inputs (or accepting
    /// caller-supplied bindings verbatim). `progress` receives the phase/timing
    /// evidence an investigation already reports.
    #[allow(clippy::too_many_arguments)]
    pub async fn start(
        client: Arc<dyn LlmClient>,
        model: impl Into<String>,
        thinking_level: Option<ThinkingLevel>,
        runtime: &ScenarioRuntime,
        supplied_inputs: Option<&HashMap<String, Value>>,
        progress: Option<Arc<Mutex<RunProgress>>>,
    ) -> Result<Self, LlmError> {
        let options = runtime.simulator.clone();
        let simulator = ToolSimulator::new(client, model, thinking_level, options);
        let mut session = simulator.session(
            &build_simulator_notes(&runtime.scenario),
            &runtime.workspace_seed,
            &runtime.implementations,
        );
        session.set_progress(progress);
        let resolved_inputs = session
            .resolve_domain(&runtime.scenario.input_domain, supplied_inputs)
            .await?;
        Ok(Self {
            session,
            tools: runtime.tools.clone(),
            world_state: Map::new(),
            resolved_inputs,
        })
    }

    pub fn resolved_inputs(&self) -> &HashMap<String, Value> {
        &self.resolved_inputs
    }

    pub fn world_state(&self) -> &Map<String, Value> {
        &self.world_state
    }

    /// Render one tool call. Unknown tools and schema-invalid arguments produce
    /// in-band tool errors (as a real framework would) without consulting the
    /// simulator at all; otherwise the supplied Lua implementation is tried
    /// first and the simulator LLM renders whatever it declines.
    pub async fn call(&mut self, name: &str, arguments: &str) -> Result<EngineCall, LlmError> {
        let Some(tool) = self.tools.iter().find(|tool| tool.name == name).cloned() else {
            return Ok(EngineCall::plain(Value::String(format!(
                "error: unknown tool '{name}'"
            ))));
        };
        let args = match validate_arguments(&tool, arguments) {
            Ok(args) => args,
            Err(error) => {
                return Ok(EngineCall::plain(Value::String(format!(
                    "error: invalid arguments: {error}"
                ))));
            }
        };
        let outcome = self
            .session
            .respond(
                &tool,
                &ToolCall {
                    name: tool.name.clone(),
                    args,
                },
                &self.world_state,
            )
            .await?;
        if let Some(patch) = outcome.state_patch {
            apply_patch(&mut self.world_state, patch);
        }
        let state_after = match tool.side_effect {
            SideEffect::Write => Some(self.world_state.clone().into_iter().collect()),
            SideEffect::Read => None,
        };
        Ok(EngineCall {
            response: outcome.response,
            state_after,
            workspace_ops: outcome.workspace_ops,
            sim_thinking: outcome.thinking,
            lua_execution: outcome.lua_execution,
        })
    }

    /// Render one call whose arguments are already JSON (probe submissions).
    pub async fn call_value(
        &mut self,
        name: &str,
        arguments: Value,
    ) -> Result<EngineCall, LlmError> {
        self.call(name, &arguments.to_string()).await
    }
}

impl EngineCall {
    /// A response produced by validation alone, with no simulator involvement.
    fn plain(response: Value) -> Self {
        Self {
            response,
            state_after: None,
            workspace_ops: Vec::new(),
            sim_thinking: None,
            lua_execution: None,
        }
    }
}

/// The simulator's system prompt notes: the caller's persona guidance, then the
/// world as explicit ground truth.
pub(crate) fn build_simulator_notes(scenario: &Scenario) -> String {
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

/// Validate a provider tool call's raw arguments the same way for investigations
/// and probes: valid JSON against the declared JSON Schema, or a readable error.
pub fn validate_arguments(tool: &ToolSchema, arguments: &str) -> Result<Value, String> {
    let args: Value =
        serde_json::from_str(arguments).map_err(|e| format!("arguments not JSON: {e}"))?;
    let validator = jsonschema::validator_for(&tool.parameters)
        .map_err(|e| format!("invalid tool schema: {e}"))?;
    validator.validate(&args).map_err(|e| e.to_string())?;
    Ok(args)
}

/// Terminal bookkeeping for a run that could not continue.
pub(crate) fn finish_progress(
    progress: &Option<Arc<Mutex<RunProgress>>>,
    stop_reason: RunStopReason,
) {
    if let Some(progress) = progress {
        if let Ok(mut progress) = progress.lock() {
            progress.finish(stop_reason);
        }
    }
}

/// A `{{variable}}` list for a template, used to check that an investigation's
/// prompt agrees with the scenario's declared inputs.
///
/// A backslash immediately before the braces (`\{{name}}`) marks literal text:
/// the run renders it as `{{name}}`, and it is not a variable (see
/// [`render_template`]).
pub fn template_variables(template: &str) -> Vec<String> {
    let mut vars: Vec<String> = Vec::new();
    let mut rest = template;
    while let Some(start) = rest.find("{{") {
        let escaped = rest[..start].ends_with('\\');
        let after = &rest[start + 2..];
        match after.find("}}") {
            Some(end) => {
                let name = after[..end].trim();
                if !escaped
                    && !name.is_empty()
                    && name.chars().all(|c| c.is_alphanumeric() || c == '_')
                    && !vars.iter().any(|v| v == name)
                {
                    vars.push(name.to_string());
                }
                rest = &after[end + 2..];
            }
            None => break,
        }
    }
    vars
}

/// The error text for a prompt whose placeholders the scenario does not declare.
pub fn missing_input_domains(
    template: &str,
    input_domain: &HashMap<String, String>,
) -> Vec<String> {
    template_variables(template)
        .into_iter()
        .filter(|name| !input_domain.contains_key(name))
        .collect()
}

/// The placeholder whose name this template escapes literally (for the error
/// text a caller sees), if any.
pub fn escaped_placeholders(template: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut rest = template;
    while let Some(start) = rest.find("\\{{") {
        let after = &rest[start + 3..];
        match after.find("}}") {
            Some(end) => {
                out.push(after[..end].to_string());
                rest = &after[end + 2..];
            }
            None => break,
        }
    }
    out
}

/// A compact, JSON-safe rendering of a call's arguments for failure messages.
pub(crate) fn summarized_args(args: &Value) -> String {
    let text = args.to_string();
    if text.len() > 200 {
        format!("{}…", &text[..200])
    } else {
        text
    }
}

/// Parse a provider tool-call's raw arguments: valid JSON as-is, otherwise the
/// raw string. Malformed arguments are the validator's concern, never a panic.
pub(crate) fn parsed_args(tc: &crate::llm::ToolCallRequest) -> Value {
    serde_json::from_str(&tc.arguments).unwrap_or(Value::String(tc.arguments.clone()))
}

/// A tool-schema-shaped default for a scenario with no declared tools.
pub fn empty_tools() -> Vec<ToolSchema> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_variables_ignores_literal_json_braces() {
        assert_eq!(
            template_variables("hello {{name}} {\"a\": 1} {{tier}} {{name}}"),
            vec!["name".to_string(), "tier".to_string()]
        );
        assert_eq!(template_variables("no placeholders"), Vec::<String>::new());
    }

    #[test]
    fn an_escaped_placeholder_is_not_a_variable() {
        // A prompt that must quote template syntax (for example one auditing a
        // template engine) writes \{{literal}} and gets literal braces.
        assert_eq!(
            template_variables("quote \\{{placeholder}} here, use {{tier}}"),
            vec!["tier".to_string()]
        );
        assert_eq!(
            missing_input_domains(
                "quote \\{{placeholder}} here",
                &HashMap::<String, String>::new()
            ),
            Vec::<String>::new()
        );
        assert_eq!(
            escaped_placeholders("quote \\{{placeholder}} here"),
            vec!["placeholder".to_string()]
        );
    }

    #[test]
    fn missing_input_domains_names_only_undeclared_placeholders() {
        let declared = HashMap::from([("tier".to_string(), "standard or premium".to_string())]);
        assert_eq!(
            missing_input_domains("{{tier}} {{order}}", &declared),
            vec!["order".to_string()]
        );
    }

    #[test]
    fn arguments_must_match_the_declared_schema() {
        use serde_json::json;
        let tool = ToolSchema {
            name: "lookup".into(),
            description: "Look up an order.".into(),
            parameters: json!({
                "type": "object",
                "properties": {"id": {"type": "string"}},
                "required": ["id"],
                "additionalProperties": false
            }),
            side_effect: SideEffect::Read,
            example_responses: vec![],
        };
        assert!(validate_arguments(&tool, r#"{"id":"O-1"}"#).is_ok());
        assert!(validate_arguments(&tool, r#"{"id":1}"#).is_err());
        assert!(validate_arguments(&tool, r#"{"other":"O-1"}"#).is_err());
        assert!(validate_arguments(&tool, "not json").is_err());
    }
}
