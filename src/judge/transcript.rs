//! Trace → transcript formatting. Shared by the LLM judge (as prompt
//! context) and by witness display (as the evidence artifact a user
//! actually reads). Product-critical: this *is* the evidence.

use crate::model::simulation::Trace;

pub fn render_transcript(trace: &Trace) -> String {
    let mut out = String::new();
    for (i, step) in trace.steps.iter().enumerate() {
        out.push_str(&format!("--- Step {i} ---\n"));
        if !step.model_output.trim().is_empty() {
            out.push_str(&format!("agent says: {}\n", step.model_output.trim()));
        }
        if let Some(tc) = &step.tool_call {
            out.push_str(&format!("tool call: {}({})\n", tc.name, tc.args));
        }
        if let Some(resp) = &step.tool_response {
            out.push_str(&format!("tool response: {resp}\n"));
        }
        if let Some(state) = &step.world_state_after {
            out.push_str(&format!(
                "world state after: {}\n",
                serde_json::to_string(state).unwrap_or_default()
            ));
        }
        out.push('\n');
    }
    out
}
