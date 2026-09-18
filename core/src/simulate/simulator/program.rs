//! Optional executable simulation: generated code is evidence, not an oracle.
//! Every attempt uses a fresh Lua VM and a staged workspace. Only validated
//! computed replies commit; explicit delegation and errors both use the LLM.
use super::*;
use crate::model::simulation::{LuaOutcome, ProgramRevision, RunPhase};
use crate::simulate::lua::{self, LuaExecution, PROGRAM_PATH};

impl SimSession {
    pub(crate) fn set_progress(&mut self, progress: Option<Arc<Mutex<RunProgress>>>) {
        self.progress = progress;
    }

    pub fn simulation_program(&self) -> Option<&SimulationProgram> {
        self.simulation_program.as_ref()
    }

    /// Prepare a fallback-only program, then let the same simulator specialize
    /// it with ordinary workspace tools. Missing implementations are allowed;
    /// no model is required to compile the entire narrative into a closed world.
    pub async fn prepare_program(&mut self, tools: &[ToolSchema]) -> Result<(), LlmError> {
        let Some(options) = self.options.lua_simulation.clone() else {
            return Ok(());
        };
        if tools.is_empty() {
            return Ok(());
        }
        if let Some(progress) = &self.progress {
            if let Ok(mut p) = progress.lock() {
                p.set_phase(RunPhase::PreparingTools);
            }
        }
        if self.workspace.file_bytes(PROGRAM_PATH).is_none() {
            self.workspace.exec(
                "write",
                &json!({"path":PROGRAM_PATH,"content":lua::fallback_source(tools)}),
            );
        }
        self.simulation_program = Some(SimulationProgram {
            path: PROGRAM_PATH.into(),
            ..Default::default()
        });
        self.capture_program();
        // Keep this contract in the conversation, including later LLM
        // fallbacks, so the simulator can specialize only selected inputs.
        self.messages.push(Message::System { content: format!(
            "EXPERIMENTAL LUA TOOL SIMULATION. The optional program at {PROGRAM_PATH} \
             implements PUT tools, not your workspace tools. It is initially a valid \
             fallback-only module. You may specialize it during preparation or later fallback \
             responses using the workspace write tool. The `.prompt-explore` namespace is \
             private harness support, not application inventory: access {PROGRAM_PATH} only to \
             author the program, never render it as a world file. The narrative remains ground \
             truth; code is a simulation artifact, not an oracle. A computed outcome means only \
             that this code executed, NOT that it faithfully implements the requested tool. You \
             do NOT need to materialize the entire world. Implement only behavior worth computing, \
             and delegate anything else. All computed and LLM responses remain in this conversation, \
             tagged with their backend.\n\n\
             LUA CONTRACT (Lua 5.4): the file returns a table mapping exact PUT tool names \
             to function(args, ctx). A function returns {{response=<any JSON-compatible value>, \
             state_patch=<object, write tools only>}}. Use ctx.world_state for current state \
             and return state_patch to change it (shallow merge; json.null deletes). The VM \
             is FRESH for every invocation: globals/upvalues are not persistent state. \
             Missing handler or PleaseSimulateException(\"reason\") delegates to you, for this \
             input only. This function RAISES the exception itself; do not return an exception \
             string/object. Ordinary Lua crashes are recorded distinctly and also delegate, \
             never disguised as tool results. All workspace mutations are staged and discarded \
             on delegation, errors or limits; only a valid computed result commits them.\n\n\
             CAPABILITIES: ctx.workspace.read(args), write(args), list_dir(args), grep(args) \
             take exactly the same argument objects and return the same result objects as your \
             workspace tools. These are the SIMULATION workspace, never the host disk. Lua \
             handlers get an application-facing view: `.prompt-explore` support files cannot be \
             listed, read, grepped, or written by ctx.workspace. Check in-band error results. \
             Your PUT tools may have DIFFERENT output contracts; adapt the returned shape faithfully. \
             The requested PUT tool's schema/description defines its semantics — do not silently \
             substitute host-capability behavior. A search described only as accepting a 'pattern' \
             is AMBIGUOUS: unless its tool description or world specifies the search grammar, \
             leave that search handler unimplemented (LLM fallback). Do not choose literal search \
             merely because the host capability is literal. In particular, ctx.workspace.grep searches a \
             LITERAL Unicode substring, not regex syntax; Lua string.find/string.match use Lua \
             PATTERNS, not regexes (for example `|` is not alternation). If requested search \
             syntax or any other tool contract cannot be implemented faithfully and safely, call \
             PleaseSimulateException(\"unsupported tool semantics\") rather than silently doing a \
             literal or Lua-pattern search. Conversely, preserve legitimate requested in-band tool \
             errors as response data; do not delegate merely because the correct response is an \
             error. Use json.null for JSON null, json.array({{}}) for an empty JSON array; ordinary \
             {{}} is an object. Source is UTF-8 text only. \
             No io, os, require/package, debug, load/dofile, pcall/xpcall/coroutines, \
             string.dump, random functions or time access. Safe basic/string/table/math \
             operations are available. Limits per invocation: {options:?}.\n\n\
             The purpose of preparation is to save future model round-trips. Prefer \
             specializing stable mechanical behavior (lookups, file operations, arithmetic), \
             even when only a subset of inputs is computable. Keep uncertain or open-ended \
             behavior delegated; leaving all handlers untouched is appropriate only when \
             there is no useful safe subset. To install an implementation you MUST write \
             the source file with the workspace write tool: code in a final JSON reply is \
             not installed. During preparation, use workspace tools as needed, then finish \
             with {{\"ready\":true}}. During later fallbacks, render the requested response as usual."
        ) });
        let result = self.ask_json::<HashMap<String, Value>>(
            format!("Prepare optional Lua implementations for these PUT tools, consistent with the world and resolved inputs already in this conversation.\n{}", serde_json::to_string_pretty(tools).expect("JSON tool schemas")),
            "{\"ready\": true}",
        ).await;
        self.capture_program();
        if let Some(program) = &mut self.simulation_program {
            program.setup_workspace_ops = std::mem::take(&mut self.workspace_ops);
            let thinking = std::mem::take(&mut self.thinking);
            program.setup_thinking = (!thinking.is_empty()).then(|| thinking.join("\n\n"));
        }
        self.publish_program();
        result.map(|_| ())
    }

    fn publish_program(&self) {
        if let (Some(program), Some(progress)) = (&self.simulation_program, &self.progress) {
            if let Ok(mut p) = progress.lock() {
                p.set_program(program.clone());
            }
        }
    }

    /// Capture every distinct source revision, never compile a truncated preview.
    /// Reading borrowed bytes avoids allocating an arbitrarily large uploaded file.
    pub(super) fn capture_program(&mut self) {
        let (Some(program), Some(options)) =
            (&mut self.simulation_program, &self.options.lua_simulation)
        else {
            return;
        };
        let revision = match self.workspace.file_bytes(PROGRAM_PATH) {
            None => ProgramRevision {
                source: String::new(),
                error: Some("simulation program file is missing".into()),
            },
            Some(bytes) if bytes.len() > options.max_source_bytes => ProgramRevision {
                source: String::from_utf8_lossy(&bytes[..options.max_source_bytes]).into_owned(),
                error: Some(format!(
                    "program has {} bytes, exceeding max_source_bytes {}; displayed source is a truncated preview and will NOT execute",
                    bytes.len(),
                    options.max_source_bytes
                )),
            },
            Some(bytes) => match std::str::from_utf8(bytes) {
                Ok(source) => ProgramRevision {
                    source: source.into(),
                    error: None,
                },
                Err(error) => ProgramRevision {
                    source: String::from_utf8_lossy(bytes).into_owned(),
                    error: Some(format!(
                        "program is not UTF-8: {error}; displayed source is a lossy preview and will NOT execute"
                    )),
                },
            },
        };
        if program.revisions.last() != Some(&revision) {
            program.revisions.push(revision);
        }
        self.publish_program();
    }

    pub(super) async fn try_lua(
        &mut self,
        tool: &ToolSchema,
        call: &ToolCall,
        world_state: &Map<String, Value>,
    ) -> (Option<SimReply>, Option<LuaExecutionRecord>) {
        self.capture_program();
        let (Some(program), Some(options)) =
            (&self.simulation_program, &self.options.lua_simulation)
        else {
            return (None, None);
        };
        let revision_index = program.revisions.len() - 1;
        let revision = &program.revisions[revision_index];
        let result = if let Some(error) = &revision.error {
            LuaExecution::Failed {
                error: error.clone(),
                operations: vec![],
            }
        } else {
            let (source, tool, call, state, workspace, options) = (
                revision.source.clone(),
                tool.clone(),
                call.clone(),
                world_state.clone(),
                self.workspace.clone(),
                options.clone(),
            );
            // An infinite loop must not block the async server's executor;
            // the sandbox independently bounds instructions, memory and time.
            tokio::task::spawn_blocking(move || {
                lua::execute(&source, &tool, &call, &state, &workspace, &options)
            })
            .await
            .unwrap_or_else(|error| LuaExecution::Failed {
                error: format!("Lua worker failed: {error}"),
                operations: vec![],
            })
        };
        let (computed, outcome, detail, discarded_workspace_ops) = match result {
            LuaExecution::Computed {
                response,
                state_patch,
                workspace,
                operations,
            } => {
                self.workspace = workspace;
                self.workspace_ops.extend(operations);
                self.capture_program();
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
                program_revision: revision_index,
                outcome,
                detail,
                discarded_workspace_ops,
            }),
        )
    }
}
