//! The simulator LLM: runs as *one persistent conversation per trace*.
//! The first turn resolves the prompt template's `{{variables}}` from the
//! scenario's `input_domain`; every later turn renders a tool call's
//! response. Folding resolution into the same (world-briefed) conversation
//! means the picked input values are consistent with the world the tools
//! will render against — and with the simulator's own later replies. Code
//! applies state patches; the LLM only proposes. When a reply is unusable,
//! the repair is a conversation message, not a parsing branch in code.
//!
//! The simulator also has a SIMULATION WORKSPACE: an in-memory filesystem
//! it accesses via four tools (read, write, list_dir, grep). Seeded from
//! an optional uploaded zip; per-trace (each run clones the seed, so
//! writes never leak across traces). The workspace is CAPABILITY, not
//! POLICY: the harness offers the tools and tells the simulator they
//! exist and are ephemeral; WHEN and WHETHER to use them — including
//! tactics like persisting generated content — is the world narrative's
//! job (the caller's words, passed through). Within one tool call the
//! simulator may make several workspace lookups before producing its
//! final JSON answer; those lookups are recorded for the trace so the
//! caller can judge whether an answer came from the filesystem or the
//! model's head.

use std::collections::HashMap;
use std::sync::Arc;

use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::llm::{ChatRequest, LlmClient, LlmError, Message, ToolCallRequest, ToolDef, parse_json};
use crate::model::ToolSchema;
use crate::model::simulation::{ToolCall, WorkspaceOp};

use super::workspace::Workspace;

/// Cap on how many workspace tool turns the simulator may take before it
/// must produce a final answer for one request. Generous enough to
/// list → grep → read several files; bounded so a stuck model cannot loop
/// forever.
const MAX_WORKSPACE_TURNS: usize = 12;

pub struct ToolSimulator {
    client: Arc<dyn LlmClient>,
    model: String,
    /// The workspace seed (uploaded zip, or empty). Cloned cheaply per
    /// trace (the seed is shared by `Arc`; only the per-trace overlay is
    /// copied), so every scenario run gets an isolated workspace.
    workspace_seed: Workspace,
}

/// What the simulator decided for one tool call.
pub struct SimOutcome {
    /// The value returned to the PUT as the tool's response.
    pub response: Value,
    /// Shallow merge into world state (null deletes a key). Present
    /// only for write-tools.
    pub state_patch: Option<Map<String, Value>>,
    /// Workspace operations the simulator performed while producing this
    /// response (transparency: lets the caller see whether the answer was
    /// grounded in the filesystem or invented).
    pub workspace_ops: Vec<WorkspaceOp>,
}

/// One simulator conversation for one trace; owns the chat history and
/// this trace's private workspace.
pub struct SimSession {
    client: Arc<dyn LlmClient>,
    model: String,
    messages: Vec<Message>,
    /// This trace's workspace: a clone of the seed with its own overlay.
    workspace: Workspace,
    /// Workspace ops accumulated since the last drain (used to attach
    /// them to the trace step they served).
    workspace_ops: Vec<WorkspaceOp>,
}

impl ToolSimulator {
    pub fn new(
        client: Arc<dyn LlmClient>,
        model: impl Into<String>,
        workspace_seed: Workspace,
    ) -> Self {
        Self {
            client,
            model: model.into(),
            workspace_seed,
        }
    }

    /// Start a simulator conversation for one scenario trace. The world
    /// specification (world + notes) is given once, up front; from then on
    /// the conversation itself is the record of what exists. The first
    /// turn (`SimSession::resolve`) picks the template's input values;
    /// later turns (`SimSession::respond`) render tool calls. The trace
    /// gets its own workspace cloned from the seed.
    pub fn session(&self, notes: &str) -> SimSession {
        let system = build_system_prompt(notes, self.workspace_seed.file_count());
        SimSession {
            client: self.client.clone(),
            model: self.model.clone(),
            messages: vec![Message::System { content: system }],
            workspace: self.workspace_seed.clone(),
            workspace_ops: Vec::new(),
        }
    }
}

impl SimSession {
    /// The first turn: pick concrete values for the template's
    /// `{{variables}}` from `input_domain`, in this world-briefed
    /// conversation (so the values are consistent with the world). Empty
    /// map when the template has no placeholders (no call). Errors if a
    /// template variable has no domain entry.
    pub async fn resolve(
        &mut self,
        template: &str,
        input_domain: &HashMap<String, String>,
    ) -> Result<HashMap<String, Value>, LlmError> {
        let vars = extract_template_vars(template);
        if vars.is_empty() {
            return Ok(HashMap::new());
        }
        if let Some(missing) = vars.iter().find(|v| !input_domain.contains_key(*v)) {
            return Err(LlmError::MalformedResponse(format!(
                "no input_domain entry for template variable '{{{missing}}}'"
            )));
        }
        let domain_block = vars
            .iter()
            .map(|v| format!("{v}: {}", input_domain[v]))
            .collect::<Vec<_>>()
            .join("\n");
        let user = format!(
            "Pick concrete values for the prompt template's variables below, consistent \
             with the WORLD SPECIFICATION above. You may consult your simulation workspace \
             if it helps (e.g. to pick a path that actually exists). Reply with a single JSON \
             object mapping each variable name to its value, and nothing else.\n\n\
             VARIABLES AND THEIR DOMAINS:\n{domain_block}"
        );
        self.ask_json::<HashMap<String, Value>>(user, "{\"<variable>\": <value>, ...}")
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

        let parsed: SimReply = self
            .ask_json(
                user,
                "{\"response\": <the tool's return value>, \"state_patch\": <write calls only>}",
            )
            .await?;
        // Drain the workspace ops accumulated for THIS response (plus any
        // left over from resolve, which had no step to attach to).
        let workspace_ops = std::mem::take(&mut self.workspace_ops);
        Ok(SimOutcome {
            response: parsed.response,
            state_patch: if is_write { parsed.state_patch } else { None },
            workspace_ops,
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
    /// shape), a repair note is appended and the whole drive is retried,
    /// up to two repair turns, then it fails loudly with the raw reply
    /// preserved. `shape` describes the required JSON for the repair note.
    async fn ask_json<T: DeserializeOwned>(
        &mut self,
        user: String,
        shape: &str,
    ) -> Result<T, LlmError> {
        self.messages.push(Message::User { content: user });
        let tools = Workspace::tool_defs();
        let mut last_failure = String::new();
        let mut last_raw = String::new();
        for attempt in 0..3 {
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
                None => last_failure = "reply was empty".into(),
                Some(content) => match parse_json::<T>(&content) {
                    Some(v) => {
                        self.messages.push(Message::Assistant {
                            content: Some(content),
                            tool_calls: vec![],
                        });
                        return Ok(v);
                    }
                    None => {
                        // Keep the malformed reply visible so the repair
                        // turn can see exactly what went wrong.
                        self.messages.push(Message::Assistant {
                            content: Some(content.clone()),
                            tool_calls: vec![],
                        });
                        last_raw = content;
                        last_failure =
                            "reply was not a single JSON object of the required shape".into();
                    }
                },
            }
        }
        Err(LlmError::MalformedResponse(format!(
            "simulator reply unusable after repair attempts ({last_failure}): {last_raw}"
        )))
    }

    /// Run the simulator with workspace tools until it produces a
    /// terminal reply (no tool calls). Tool calls are executed against
    /// this trace's workspace and fed back as tool messages. Returns the
    /// terminal content (None if empty). Bounded by `MAX_WORKSPACE_TURNS`.
    async fn run_workspace_loop(&mut self, tools: &[ToolDef]) -> Result<Option<String>, LlmError> {
        let mut turns = 0;
        loop {
            if turns >= MAX_WORKSPACE_TURNS {
                return Err(LlmError::MalformedResponse(format!(
                    "simulator made more than {MAX_WORKSPACE_TURNS} workspace tool calls \
                     without a final answer"
                )));
            }
            let reply = self
                .client
                .complete(ChatRequest {
                    model: self.model.clone(),
                    messages: self.messages.clone(),
                    tools: tools.to_vec(),
                    temperature: Some(0.7),
                    // Reasoning-style models can burn a small budget on
                    // hidden reasoning and return empty content.
                    max_tokens: Some(8192),
                })
                .await
                .map_err(|e| LlmError::Provider(e.to_string()))?;

            if reply.tool_calls.is_empty() {
                // Terminal: this content is the candidate final JSON.
                return Ok(reply.content.filter(|c| !c.trim().is_empty()));
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

#[derive(Deserialize)]
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
         • The FIRST request asks you to pick concrete values for the prompt \
         template's {{variables}} from their input domains — reply with a JSON \
         object mapping each variable name to its value (strings unless the domain \
         implies structure; quote large blocks verbatim, do not paraphrase).\n\
         • Every LATER request describes one tool call — reply with \
         {{\"response\": <the tool's return value>, \"state_patch\": <write calls \
         only>}}.\n\n\
         YOUR SIMULATION WORKSPACE. You also have a simulation workspace: an \
         in-memory filesystem private to this run. You access it with four tools — \
         list_dir, read, grep, write — and have free rein to use them however helps \
         you produce faithful, consistent responses (look up real contents, search \
         across files, record generated content so later re-reads stay consistent). \
         {boot_line} The workspace is EPHEMERAL: it exists only for this run, every \
         run starts fresh from the same seed, and the agent you are simulating NEVER \
         sees it — only your tool responses reach it. So everything that agent needs \
         must be IN your response, never merely 'saved to disk'. Call workspace \
         tools as needed; when you are ready, give your FINAL answer as the JSON \
         object above with NO tool calls.\n\n\
         Your earlier replies in this conversation are the established record of the \
         environment: every response MUST be consistent with them (same files, same \
         contents, same facts — what has been read stays read; the input values you \
         picked stay picked). The WORLD SPECIFICATION below is ground truth: render \
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

/// Extract `{{variable}}` placeholder names from a template. Names are
/// alphanumeric/underscore only, so literal JSON braces in a template
/// (e.g. `{"a": ...}`) are not mistaken for placeholders.
fn extract_template_vars(template: &str) -> Vec<String> {
    let mut vars: Vec<String> = Vec::new();
    let mut rest = template;
    while let Some(start) = rest.find("{{") {
        let after = &rest[start + 2..];
        match after.find("}}") {
            Some(end) => {
                let name = after[..end].trim();
                if !name.is_empty()
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
