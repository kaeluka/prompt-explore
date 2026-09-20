//! The simulator LLM: runs as *one persistent conversation per trace*.
//! The first turn resolves the scenario's declared `input_domain` (or accepts
//! the caller's explicit bindings); every later turn renders a tool call's
//! response. Folding resolution into the same (world-briefed) conversation
//! means the picked input values are consistent with the world the tools
//! will render against — and with the simulator's own later replies. Code
//! applies state patches; the LLM only proposes. When a reply is unusable,
//! the repair is a conversation message, not a parsing branch in code.
//!
//! A tool with a CALLER-SUPPLIED Lua implementation is tried in the sandbox
//! first; the harness never authors or rewrites that code. Computed responses
//! enter this same conversation, delegation and sandbox errors fall through to
//! the model, and every attempt is recorded as evidence.
//!
//! The simulator also has a SIMULATION WORKSPACE: an in-memory filesystem
//! it accesses via four tools (read, write, list_dir, grep). Seeded from
//! an optional uploaded archive; per-run (each run clones the seed, so
//! writes never leak across runs). The workspace is CAPABILITY, not
//! POLICY: the harness offers the tools and tells the simulator they
//! exist and are ephemeral; WHEN and WHETHER to use them — including
//! tactics like persisting generated content — is the world narrative's
//! job (the caller's words, passed through). Within one tool call the
//! simulator may make several workspace lookups before producing its
//! final JSON answer; those lookups are recorded for the trace so the
//! caller can judge whether an answer came from the filesystem or the
//! model's head.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value, json};

use crate::llm::{
    ChatRequest, LlmClient, LlmError, Message, ThinkingLevel, ToolCallRequest, ToolDef,
    parse::parse_json_with_error,
};
use crate::model::ToolSchema;
use crate::model::scenario::ToolImplementation;
use crate::model::simulation::{
    LuaExecutionRecord, LuaOutcome, RunProgress, ToolCall, WorkspaceOp,
};

use super::lua::{self, LuaExecution, LuaOptions};

use super::workspace::Workspace;

/// Cap on how many workspace tool turns the simulator may take before it
/// must produce a final answer for one request. Generous enough to
/// list → grep → read several files; bounded so a stuck model cannot loop
/// forever. Configurable via the
/// `PROMPT_EXPLORE_MAX_WORKSPACE_TURNS` environment variable (default 250).
pub const DEFAULT_MAX_WORKSPACE_TURNS: usize = 250;
/// Default sampling temperature for simulator completions.
pub const DEFAULT_SIMULATOR_TEMPERATURE: f32 = 0.7;
/// Default output-token limit for each simulator completion.
pub const DEFAULT_SIMULATOR_MAX_TOKENS: u32 = 32 * 1024;
/// Default number of total attempts for a malformed simulator reply
/// (the initial reply plus repairs).
pub const DEFAULT_SIMULATOR_REPAIR_ATTEMPTS: usize = 20;

/// Controls for the simulator's LLM conversation. Supply these at the
/// runner boundary rather than embedding policy in the conversation loop.
#[derive(Debug, Clone)]
pub struct SimulatorOptions {
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
    /// Total attempts per JSON reply (initial attempt included); default 20.
    /// Applies to empty replies, invalid JSON, and schema mismatches.
    pub max_repair_attempts: usize,
    pub max_workspace_turns: usize,
    /// Experimental executable simulation. None retains pure LLM simulation;
    /// Some(default options) enables Lua setup and per-call computed/fallback routing.
    pub lua_simulation: Option<LuaOptions>,
}

impl SimulatorOptions {
    /// Resolve a scenario's simulator settings into concrete options. Every
    /// `None` becomes a named default here, so no limit is a hidden constant.
    /// Lua runs exactly when the scenario supplies an implementation.
    pub fn from_settings(
        settings: &crate::model::scenario::SimulationSettings,
        lua_enabled: bool,
    ) -> Self {
        Self {
            temperature: settings.temperature.or(Some(DEFAULT_SIMULATOR_TEMPERATURE)),
            max_tokens: settings.max_tokens.or(Some(DEFAULT_SIMULATOR_MAX_TOKENS)),
            max_repair_attempts: settings
                .max_repair_attempts
                .unwrap_or(DEFAULT_SIMULATOR_REPAIR_ATTEMPTS),
            max_workspace_turns: settings
                .max_workspace_turns
                .unwrap_or(DEFAULT_MAX_WORKSPACE_TURNS),
            lua_simulation: lua_enabled.then(|| settings.lua.clone().unwrap_or_default()),
        }
    }
}

impl Default for SimulatorOptions {
    fn default() -> Self {
        Self {
            temperature: Some(DEFAULT_SIMULATOR_TEMPERATURE),
            max_tokens: Some(DEFAULT_SIMULATOR_MAX_TOKENS),
            max_repair_attempts: DEFAULT_SIMULATOR_REPAIR_ATTEMPTS,
            max_workspace_turns: DEFAULT_MAX_WORKSPACE_TURNS,
            lua_simulation: None,
        }
    }
}

pub struct ToolSimulator {
    client: Arc<dyn LlmClient>,
    model: String,
    /// Thinking level for every simulator completion in every trace;
    /// `None` = the provider's default (no field sent).
    thinking_level: Option<ThinkingLevel>,
    options: SimulatorOptions,
}

/// What the simulator decided for one tool call.
pub struct SimOutcome {
    pub lua_execution: Option<LuaExecutionRecord>,
    /// The value returned to the PUT as the tool's response.
    pub response: Value,
    /// Shallow merge into world state (null deletes a key). Present
    /// only for write-tools.
    pub state_patch: Option<Map<String, Value>>,
    /// Workspace operations the simulator performed while producing this
    /// response (transparency: lets the caller see whether the answer was
    /// grounded in the filesystem or invented).
    pub workspace_ops: Vec<WorkspaceOp>,
    /// The simulator model's visible reasoning while producing this
    /// response (all inner-drive turns concatenated). Transparency only.
    pub thinking: Option<String>,
}

/// One simulator conversation for one trace; owns the chat history and
/// this trace's private workspace.
pub struct SimSession {
    /// The scenario's caller-supplied Lua implementations, if any. Empty leaves
    /// every tool to the simulator LLM. Never generated or rewritten here.
    implementations: Vec<ToolImplementation>,
    progress: Option<Arc<Mutex<RunProgress>>>,
    client: Arc<dyn LlmClient>,
    model: String,
    thinking_level: Option<ThinkingLevel>,
    messages: Vec<Message>,
    options: SimulatorOptions,
    /// This trace's workspace: a clone of the seed with its own overlay.
    workspace: Workspace,
    /// Workspace ops accumulated since the last drain (used to attach
    /// them to the trace step they served).
    workspace_ops: Vec<WorkspaceOp>,
    /// The simulator model's reasoning turns accumulated since the last
    /// drain, same lifetime rule as `workspace_ops`.
    thinking: Vec<String>,
}

impl ToolSimulator {
    pub fn new(
        client: Arc<dyn LlmClient>,
        model: impl Into<String>,
        thinking_level: Option<ThinkingLevel>,
        options: SimulatorOptions,
    ) -> Self {
        Self {
            client,
            model: model.into(),
            thinking_level,
            options,
        }
    }

    /// Start a simulator conversation for one scenario run. The world
    /// specification (world + notes) is given once, up front; from then on
    /// the conversation itself is the record of what exists. The first
    /// turn (`SimSession::resolve`) picks the template's input values;
    /// later turns (`SimSession::respond`) render tool calls. The trace
    /// gets its own workspace cloned from the seed.
    pub fn session(
        &self,
        notes: &str,
        workspace_seed: &Workspace,
        implementations: &[ToolImplementation],
    ) -> SimSession {
        let system = build_system_prompt(notes, workspace_seed.file_count());
        SimSession {
            implementations: implementations.to_vec(),
            progress: None,
            client: self.client.clone(),
            model: self.model.clone(),
            thinking_level: self.thinking_level,
            messages: vec![Message::System { content: system }],
            workspace: workspace_seed.clone(),
            options: self.options.clone(),
            workspace_ops: Vec::new(),
            thinking: Vec::new(),
        }
    }
}

impl SimSession {
    pub(crate) fn set_progress(&mut self, progress: Option<Arc<Mutex<RunProgress>>>) {
        self.progress = progress;
    }

    /// Try the scenario's caller-supplied implementation for this tool, when it
    /// has one. No implementation means no Lua attempt at all, and the caller
    /// renders the response with a single LLM call.
    ///
    /// The narrative remains ground truth: `Computed` means the code ran, not
    /// that it implemented the declared contract faithfully. A `Fallback` or
    /// `Error` is NOT the tool's return value — the staged workspace writes are
    /// discarded and the simulator LLM renders the actual response.
    async fn try_lua(
        &mut self,
        tool: &ToolSchema,
        call: &ToolCall,
        world_state: &Map<String, Value>,
    ) -> (Option<SimReply>, Option<LuaExecutionRecord>) {
        let tool_name = tool.name.clone();
        let Some(implementation) = self
            .implementations
            .iter()
            .find(|implementation| implementation.tool == tool_name)
            .cloned()
        else {
            return (None, None);
        };
        let Some(options) = self.options.lua_simulation.clone() else {
            return (None, None);
        };
        let source_hash = implementation.source_hash.clone();
        let (source, tool, call, state, workspace) = (
            implementation.source,
            tool.clone(),
            call.clone(),
            world_state.clone(),
            self.workspace.clone(),
        );
        // An infinite loop must not block the async server's executor; the
        // sandbox independently bounds instructions, memory and time.
        let result = tokio::task::spawn_blocking(move || {
            lua::execute(&source, &tool, &call, &state, &workspace, &options)
        })
        .await
        .unwrap_or_else(|error| LuaExecution::Failed {
            error: format!("Lua worker failed: {error}"),
            operations: vec![],
        });
        let (computed, outcome, detail, discarded_workspace_ops) = match result {
            LuaExecution::Computed {
                response,
                state_patch,
                workspace,
                operations,
            } => {
                self.workspace = workspace;
                self.workspace_ops.extend(operations);
                (
                    Some(SimReply {
                        response,
                        state_patch,
                    }),
                    LuaOutcome::Computed,
                    None,
                    vec![],
                )
            }
            LuaExecution::Fallback { reason, operations } => {
                (None, LuaOutcome::Fallback, Some(reason), operations)
            }
            LuaExecution::Failed { error, operations } => {
                (None, LuaOutcome::Error, Some(error), operations)
            }
        };
        (
            computed,
            Some(LuaExecutionRecord {
                tool: tool_name,
                source_hash,
                outcome,
                detail,
                discarded_workspace_ops,
            }),
        )
    }

    /// The first turn: pick concrete values for the scenario's declared
    /// `input_domain`, in this world-briefed conversation (so the values are
    /// consistent with the world). When `supplied` is given, those bindings are
    /// used verbatim (a testable/replayable selection) and only their shape is
    /// validated; no model call happens. Empty domain and no bindings = no call.
    pub async fn resolve_domain(
        &mut self,
        input_domain: &HashMap<String, String>,
        supplied: Option<&HashMap<String, Value>>,
    ) -> Result<HashMap<String, Value>, LlmError> {
        if let Some(supplied) = supplied {
            let unknown: Vec<&String> = supplied
                .keys()
                .filter(|key| !input_domain.contains_key(*key))
                .collect();
            if !unknown.is_empty() {
                return Err(LlmError::MalformedResponse(format!(
                    "resolved_inputs names input(s) the scenario does not declare: {}",
                    unknown
                        .iter()
                        .map(|k| format!("'{k}'"))
                        .collect::<Vec<_>>()
                        .join(", ")
                )));
            }
            let missing: Vec<&String> = input_domain
                .keys()
                .filter(|key| !supplied.contains_key(*key))
                .collect();
            if !missing.is_empty() {
                return Err(LlmError::MalformedResponse(format!(
                    "resolved_inputs omits declared input(s): {} — supply every declared key or omit \
                     resolved_inputs entirely to sample one",
                    missing
                        .iter()
                        .map(|k| format!("'{k}'"))
                        .collect::<Vec<_>>()
                        .join(", ")
                )));
            }
            return Ok(supplied.clone());
        }
        if input_domain.is_empty() {
            return Ok(HashMap::new());
        }
        let mut names: Vec<&String> = input_domain.keys().collect();
        names.sort();
        let domain_block = names
            .iter()
            .map(|name| format!("{name}: {}", input_domain[*name]))
            .collect::<Vec<_>>()
            .join("\n");
        let user = format!(
            "Pick concrete values for every declared input below, consistent \
             with the WORLD SPECIFICATION above. You may consult your simulation workspace \
             if it helps (e.g. to pick a path that actually exists). Reply with a single JSON \
             object mapping each input name to its value, and nothing else.\n\n\
             DECLARED INPUTS AND THEIR DOMAINS:\n{domain_block}"
        );
        self.ask_json::<HashMap<String, Value>>(user, "{\"<input>\": <value>, ...}")
            .await
    }

    /// A later turn: render one tool call's response. Appends the call as
    /// a user message, gets the reply (letting the simulator consult the
    /// workspace as needed), applies any state patch. Returns the tool
    /// response and the workspace ops performed along the way.
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

        let (computed, lua_execution) = self.try_lua(tool, call, world_state).await;
        let parsed: SimReply = if let Some(parsed) = computed {
            // Same user request / assistant completion shape as the LLM path.
            // No model call: future fallbacks still see the entire established
            // history, including computed responses and their provenance.
            let mut request: Value = serde_json::from_str(&user).expect("generated JSON");
            request["execution_backend"] = json!("lua");
            self.messages.push(Message::User {
                content: request.to_string(),
            });
            self.messages.push(Message::Assistant {
                content: Some(serde_json::to_string(&parsed).expect("JSON simulator reply")),
                tool_calls: vec![],
            });
            parsed
        } else {
            let mut request: Value = serde_json::from_str(&user).expect("generated JSON");
            if let Some(record) = &lua_execution {
                request["lua_attempt"] = serde_json::to_value(record).expect("JSON Lua record");
                request["lua_instructions"] = json!(format!(
                    "Your caller-supplied Lua implementation DECLINED this call{}, so no workspace \
                     write or world-state patch was committed. Declining is a normal, successful \
                     outcome — not a bug to work around. Render this response now from the world and \
                     the established conversation.",
                    lua_execution
                        .as_ref()
                        .and_then(|record| record.detail.as_deref())
                        .map(|detail| format!(" ({detail})"))
                        .unwrap_or_default()
                ));
            }
            let request_content = if lua_execution.is_some() {
                request.to_string()
            } else {
                user
            };
            self.ask_json(
                request_content,
                "{\"response\": <the tool's return value>, \"state_patch\": <write calls only>}",
            )
            .await?
        };
        // Drain the workspace ops and simulator thinking accumulated for
        // THIS response (plus any left over from resolve, which had no step
        // to attach to).
        let workspace_ops = std::mem::take(&mut self.workspace_ops);
        let thinking = {
            let parts = std::mem::take(&mut self.thinking);
            (!parts.is_empty()).then(|| parts.join("\n\n"))
        };
        Ok(SimOutcome {
            lua_execution,
            response: parsed.response,
            state_patch: if is_write { parsed.state_patch } else { None },
            workspace_ops,
            thinking,
        })
    }

    /// Execute one workspace tool call from the simulator against this
    /// trace's workspace, recording it for the trace.
    fn exec_workspace(&mut self, tc: &ToolCallRequest) -> Value {
        let args: Value = serde_json::from_str(&tc.arguments).unwrap_or(json!({}));
        let result = self.workspace.exec(&tc.name, &args);
        self.workspace_ops.push(WorkspaceOp {
            tool: tc.name.clone(),
            args,
            result: result.clone(),
        });
        result
    }

    /// Push a user message, then drive the simulator to a final JSON
    /// answer. The simulator may make workspace tool calls first (an inner
    /// loop): each round with tool calls is executed against the workspace
    /// and fed back; a round with NO tool calls is the terminal candidate,
    /// parsed as the answer. On an unusable terminal reply (empty or wrong
    /// shape), a repair note is appended and the whole drive is retried up
    /// to the configured attempt limit, then fails loudly with the raw reply
    /// preserved. `shape` describes the required JSON for the repair note.
    async fn ask_json<T: DeserializeOwned>(
        &mut self,
        user: String,
        shape: &str,
    ) -> Result<T, LlmError> {
        self.messages.push(Message::User { content: user });
        let tools = self.workspace.tool_defs();
        let mut last_failure = String::new();
        let mut last_raw = String::new();
        for attempt in 0..self.options.max_repair_attempts {
            if attempt > 0 {
                // NOTE: interleaved system messages are fine on
                // OpenAI-compatible providers (z.ai, OpenRouter). On
                // providers whose API has a single top-level system param
                // (Bedrock/Anthropic), genai merges system messages, so
                // this repair note loses its position but not its content.
                self.messages.push(Message::System {
                    content: format!(
                        "Your previous reply could not be used: {last_failure}. \
                         Reply again with a single JSON object of the form {shape} \
                         and nothing else."
                    ),
                });
            }
            // Inner workspace loop: run until a terminal (no tool calls)
            // reply, executing any workspace lookups along the way.
            let terminal = self.run_workspace_loop(&tools).await?;
            match terminal {
                None => {
                    last_failure = "reply was empty".into();
                    last_raw.clear();
                }
                Some(content) => match parse_json_with_error::<T>(&content) {
                    Ok(v) => {
                        self.messages.push(Message::Assistant {
                            content: Some(content),
                            tool_calls: vec![],
                        });
                        return Ok(v);
                    }
                    Err(error) => {
                        // Keep the malformed reply visible so the repair
                        // turn can see exactly what went wrong.
                        self.messages.push(Message::Assistant {
                            content: Some(content.clone()),
                            tool_calls: vec![],
                        });
                        last_raw = content;
                        last_failure = format!(
                            "reply was not a single JSON object of the required shape: {error}"
                        );
                    }
                },
            }
        }
        Err(LlmError::MalformedResponse(format!(
            "simulator reply unusable after {} attempts ({last_failure}): {last_raw}",
            self.options.max_repair_attempts,
        )))
    }

    /// Run the simulator with workspace tools until it produces a
    /// terminal reply (no tool calls). Tool calls are executed against
    /// this trace's workspace and fed back as tool messages. Returns the
    /// terminal content (None if empty). Bounded by `max_workspace_turns`.
    /// When the cap is reached the harness first nudges the simulator to
    /// produce its final answer from what it has already seen; only if it
    /// still calls tools after the nudge does the call fail.
    async fn run_workspace_loop(&mut self, tools: &[ToolDef]) -> Result<Option<String>, LlmError> {
        let mut turns = 0;
        loop {
            if turns >= self.options.max_workspace_turns {
                // Graceful fallback: nudge the simulator to produce its
                // final answer from what it has already seen. Only
                // escalate to an error if it still calls tools after
                // this nudge — the caller can then raise the cap via
                // PROMPT_EXPLORE_MAX_WORKSPACE_TURNS.
                let max = self.options.max_workspace_turns;
                self.messages.push(Message::System {
                    content: format!(
                        "You have used your {max} workspace lookups for this response. \
                         Produce your FINAL answer as the JSON object now, from what \
                         you have already seen. Do NOT call any workspace tools."
                    ),
                });
                let reply = self
                    .client
                    .complete(ChatRequest {
                        model: self.model.clone(),
                        messages: self.messages.clone(),
                        tools: tools.to_vec(),
                        temperature: self.options.temperature,
                        max_tokens: self.options.max_tokens,
                        thinking_level: self.thinking_level,
                    })
                    .await
                    .map_err(|e| LlmError::Provider(e.to_string()))?;

                if reply.tool_calls.is_empty() {
                    if let Some(t) = &reply.thinking {
                        if !t.trim().is_empty() {
                            self.thinking.push(t.clone());
                        }
                    }
                    return Ok(reply.content.filter(|c| !c.trim().is_empty()));
                }

                // Still making tool calls — escalate to error.
                self.messages.push(Message::Assistant {
                    content: reply.content.clone(),
                    tool_calls: reply.tool_calls.clone(),
                });
                return Err(LlmError::MalformedResponse(format!(
                    "simulator made more than {max} workspace tool calls \
                     without a final answer (even after a nudge to stop)"
                )));
            }
            let reply = self
                .client
                .complete(ChatRequest {
                    model: self.model.clone(),
                    messages: self.messages.clone(),
                    tools: tools.to_vec(),
                    temperature: self.options.temperature,
                    // Reasoning-style models can burn a small budget on
                    // hidden reasoning and return empty content.
                    max_tokens: self.options.max_tokens,
                    thinking_level: self.thinking_level,
                })
                .await
                .map_err(|e| LlmError::Provider(e.to_string()))?;

            if reply.tool_calls.is_empty() {
                // Terminal: this content is the candidate final JSON.
                if let Some(t) = &reply.thinking {
                    if !t.trim().is_empty() {
                        self.thinking.push(t.clone());
                    }
                }
                return Ok(reply.content.filter(|c| !c.trim().is_empty()));
            }

            if let Some(t) = &reply.thinking {
                if !t.trim().is_empty() {
                    self.thinking.push(t.clone());
                }
            }

            // The simulator made workspace tool calls. Append the assistant
            // turn (with the calls) first, then each tool result — the
            // OpenAI conversation convention the providers expect.
            self.messages.push(Message::Assistant {
                content: reply.content.clone(),
                tool_calls: reply.tool_calls.clone(),
            });
            for tc in &reply.tool_calls {
                let result = self.exec_workspace(tc);
                self.messages.push(Message::Tool {
                    tool_call_id: tc.id.clone(),
                    content: result.to_string(),
                });
            }
            turns += 1;
        }
    }
}

#[derive(Deserialize, serde::Serialize)]
struct SimReply {
    response: Value,
    state_patch: Option<Map<String, Value>>,
}

/// Build the simulator's system prompt: the resolve/respond contract,
/// the simulation-workspace briefing (name, tools, boot line, ephemerality,
/// free rein — all capability, no policy), and the world specification.
fn build_system_prompt(notes: &str, workspace_files: usize) -> String {
    let boot_line: String = if workspace_files == 0 {
        "It currently contains 0 files (it is empty — nothing was uploaded to \
         seed it; you may still use the write tool as scratch memory)."
            .to_string()
    } else {
        format!("It currently contains {workspace_files} file(s).")
    };
    format!(
        "You are simulating software tools inside an agent test harness. You answer \
         a sequence of requests in ONE conversation. Each FINAL answer is a single \
         JSON object and nothing else:\n\
         • If a request asks you to pick concrete values for the prompt template's \
         {{variables}}, it is input resolution. Reply with a JSON object mapping each \
         variable name to its value (strings unless the domain implies structure; quote \
         large blocks verbatim, do not paraphrase). This request occurs only when the \
         template has placeholders, so the first request may instead be a tool call.\n\
         • Every tool-call request — whether first or later — requires \
         {{\"response\": <the tool's return value>, \"state_patch\": <write calls \
         only>}}.\n\n\
         YOUR SIMULATION WORKSPACE. You also have a simulation workspace: an \
         in-memory filesystem private to this run. You access it with four tools — \
         list_dir, read, grep, write — and have free rein to use them however helps \
         you produce faithful, consistent responses (look up real contents, search \
         across files, record generated content so later re-reads stay consistent). \
         {boot_line} The workspace is EPHEMERAL: it exists only for this run, every \
         run starts fresh from the same seed, and the agent you are simulating NEVER \
         sees it — only your tool responses reach it. The reserved `.prompt-explore` \
         namespace is harness support and is NOT part of the \
         application's world inventory, so never render it as an application file or \
         world fact. So everything that agent needs must be \
         IN your response, never merely 'saved to disk'. Call workspace \
         tools as needed; when you are ready, give your FINAL answer as the JSON \
         object above with NO tool calls.\n\n\
         Your earlier replies in this conversation are the established record of the \
         environment: every response MUST be consistent with them (same files, same \
         contents, same facts — what has been read stays read; the input values you \
         picked stay picked). This includes the RESPONSE ENVELOPE. The tool's declared \
         description and `example_responses`, together with the world, DEFINE its return \
         shape, and that declaration WINS over what a workspace lookup happens to return: \
         if a declared shape or example exists, reshape your answer to match it exactly — \
         same keys, same nesting, no extra or missing fields — even when the value you looked \
         up came back in a different form. Forwarding the workspace result object unchanged \
         when the tool declares a DIFFERENT shape is a defect, not faithfulness. Only when \
         the declared contract specifies no shape at all may you pass the workspace result \
         through unchanged, and then you must keep that shape identical for every call of \
         that tool (never a bare array in one call and an object in the next) — a downstream \
         agent may parse the envelope, so a shape that varies between calls or runs breaks it \
         even when each individual shape looks reasonable. The WORLD SPECIFICATION below is ground truth: render \
         responses and choose input values consistent with it, refuse queries for \
         things it says do not exist or that its inventory does not cover, and never \
         introduce facts that contradict it. Filler for unspecified content must \
         introduce no new facts.\n\n\
         WORLD SPECIFICATION AND NOTES:\n{notes}"
    )
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
