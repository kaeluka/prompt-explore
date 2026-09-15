//! Trace → transcript formatting. A pure display utility: this is the
//! evidence artifact a caller (human or LLM) actually reads when judging
//! a trace. Shared by examples and any consumer that wants a readable
//! rendering of a `Trace`. No judging happens here.

use crate::model::simulation::Trace;

pub fn render_transcript(trace: &Trace) -> String {
    let mut out = String::new();
    if let Some(program) = &trace.simulation_program {
        out.push_str(&format!("--- simulation program: {} ---\n", program.path));
        for (revision, source) in program.revisions.iter().enumerate() {
            out.push_str(&format!("revision {revision}:\n{}\n", source.source));
            if let Some(error) = &source.error {
                out.push_str(&format!("program error: {error}\n"));
            }
        }
        if !program.setup_workspace_ops.is_empty() {
            out.push_str(&format!(
                "setup workspace operations: {}\n",
                serde_json::to_string(&program.setup_workspace_ops).unwrap_or_default()
            ));
        }
        if let Some(thinking) = &program.setup_thinking {
            out.push_str(&format!("setup thinking: {thinking}\n"));
        }
    }
    for (i, turn) in trace.turns.iter().enumerate() {
        out.push_str(&format!("--- PUT turn {} ---\n", i + 1));
        if !turn.model_output.trim().is_empty() {
            out.push_str(&format!("agent says: {}\n", turn.model_output.trim()));
        }
        if turn.tool_exchanges.len() > 1 {
            out.push_str(&format!(
                "tool batch: {} calls requested in this completion\n",
                turn.tool_exchanges.len()
            ));
        }
        for (j, exchange) in turn.tool_exchanges.iter().enumerate() {
            let prefix = if turn.tool_exchanges.len() > 1 {
                format!("[{}/{}] ", j + 1, turn.tool_exchanges.len())
            } else {
                String::new()
            };
            out.push_str(&format!(
                "{prefix}tool call: {}({})\n",
                exchange.call.name, exchange.call.args
            ));
            if let Some(lua) = &exchange.lua_execution {
                out.push_str(&format!(
                    "{prefix}Lua attempt: {}\n",
                    serde_json::to_string(lua).unwrap_or_default()
                ));
            }
            out.push_str(&format!("{prefix}tool response: {}\n", exchange.response));
            if let Some(state) = &exchange.world_state_after {
                out.push_str(&format!(
                    "{prefix}world state after: {}\n",
                    serde_json::to_string(state).unwrap_or_default()
                ));
            }
        }
        out.push('\n');
    }
    out
}
