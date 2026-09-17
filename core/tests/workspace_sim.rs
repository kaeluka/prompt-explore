//! The simulator's workspace inner loop: when the simulator issues a
//! workspace tool call before its final answer, the harness executes it
//! against the per-trace workspace, feeds the result back, and records
//! the operation in the trace. Deterministic (scripted models, no network).

use std::io::Write;
use std::sync::Arc;

use serde_json::json;

use prompt_explore::llm::{ChatResponse, MockLlmClient, ToolCallRequest};
use prompt_explore::model::*;
use prompt_explore::simulate::{Runner, RunnerOptions, Workspace, unpack_zip};

/// Build a zip entirely in memory and unpack it into a workspace seeded
/// with `src/main.rs` -> "fn main() {}".
fn seeded_workspace() -> Workspace {
    let mut buf: Vec<u8> = Vec::new();
    {
        let mut zw = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts = zip::write::SimpleFileOptions::default();
        zw.start_file("src/main.rs", opts).unwrap();
        zw.write_all(b"fn main() {}").unwrap();
        zw.finish().unwrap();
    }
    unpack_zip(&buf).expect("unpack seeded zip")
}

#[tokio::test]
async fn simulator_consults_workspace_then_records_op_in_trace() {
    // PUT model: calls get_code once, then stops with a text reply.
    let put_model = MockLlmClient::scripted(vec![
        ChatResponse {
            content: None,
            thinking: None,
            tool_calls: vec![ToolCallRequest {
                id: "c1".into(),
                name: "get_code".into(),
                arguments: r#"{"path":"src/main.rs"}"#.into(),
            }],
            usage: None,
        },
        ChatResponse {
            content: Some("here is the code".into()),
            thinking: None,
            tool_calls: vec![],
            usage: None,
        },
    ]);
    // Simulator: first turn asks the workspace to read the file; second
    // turn produces the terminal JSON answer (grounded in what it read).
    let sim_model = MockLlmClient::scripted(vec![
        ChatResponse {
            content: None,
            thinking: None,
            tool_calls: vec![ToolCallRequest {
                id: "w1".into(),
                name: "read".into(),
                arguments: r#"{"path":"src/main.rs"}"#.into(),
            }],
            usage: None,
        },
        ChatResponse {
            content: Some(r#"{"response": "fn main() {}"}"#.into()),
            thinking: None,
            tool_calls: vec![],
            usage: None,
        },
    ]);

    let put = PromptUnderTest {
        id: "reader".into(),
        template: "You are a code reader.".into(),
        tools: vec![ToolSchema {
            name: "get_code".into(),
            description: "Get a file's contents.".into(),
            parameters: json!({
                "type": "object",
                "properties": { "path": { "type": "string" } },
                "required": ["path"]
            }),
            side_effect: SideEffect::Read,
            example_responses: vec![],
        }],
        design_goals: "return the file contents".into(),
    };
    let scenario = Scenario {
        world: "The workspace contains the repo under test.".into(),
        input_domain: Default::default(),
        user_message: Some("show me src/main.rs".into()),
        simulator_notes: String::new(),
    };
    let budget = Budget {
        max_steps_per_trace: 6,
        max_tokens: None,
    };

    let runner = Runner::new(
        Arc::new(put_model),
        "put-model",
        None,
        Arc::new(sim_model),
        "sim-model",
        None,
        seeded_workspace(),
        RunnerOptions::default(),
    );
    let trace = runner.run(&put, &scenario, &budget, None).await.unwrap();

    // The first turn's tool exchange must carry the workspace read the
    // simulator performed, with the REAL seeded content as the result.
    let exchange = &trace.turns[0].tool_exchanges[0];
    let read_op = exchange
        .workspace_ops
        .iter()
        .find(|o| o.tool == "read")
        .expect("the simulator's read should be recorded in the trace");
    assert_eq!(read_op.args["path"], json!("src/main.rs"));
    assert_eq!(read_op.result["content"], json!("fn main() {}"));
    // The simulated tool response is the simulator's final answer.
    assert_eq!(exchange.response, json!("fn main() {}"));
}

#[tokio::test]
async fn empty_workspace_runs_normally_without_tool_calls() {
    // A simulator that answers immediately (no workspace lookups) works
    // exactly as before: no ops recorded, one sim call consumed.
    let put_model = MockLlmClient::scripted(vec![
        ChatResponse {
            content: None,
            thinking: None,
            tool_calls: vec![ToolCallRequest {
                id: "c1".into(),
                name: "ping".into(),
                arguments: "{}".into(),
            }],
            usage: None,
        },
        ChatResponse {
            content: Some("ok".into()),
            thinking: None,
            tool_calls: vec![],
            usage: None,
        },
    ]);
    let sim_model = MockLlmClient::scripted(vec![ChatResponse {
        content: Some(r#"{"response": "pong"}"#.into()),
        thinking: None,
        tool_calls: vec![],
        usage: None,
    }]);

    let put = PromptUnderTest {
        id: "p".into(),
        template: "ping agent".into(),
        tools: vec![ToolSchema {
            name: "ping".into(),
            description: "ping".into(),
            parameters: json!({"type": "object"}),
            side_effect: SideEffect::Read,
            example_responses: vec![],
        }],
        design_goals: "".into(),
    };
    let scenario = Scenario {
        world: "trivial".into(),
        input_domain: Default::default(),
        user_message: Some("ping".into()),
        simulator_notes: String::new(),
    };
    let runner = Runner::new(
        Arc::new(put_model),
        "put-model",
        None,
        Arc::new(sim_model),
        "sim-model",
        None,
        Workspace::empty(),
        RunnerOptions::default(),
    );
    let trace = runner.run(&put, &scenario, &budget(), None).await.unwrap();
    assert!(trace.turns[0].tool_exchanges[0].workspace_ops.is_empty());
}

#[tokio::test]
async fn direct_runner_calls_get_isolated_workspace_overlays() {
    let put_model = MockLlmClient::scripted(vec![
        tool_call("first"),
        final_reply(),
        tool_call("second"),
        final_reply(),
    ]);
    // The first trace writes scratch state. The second trace attempts to read
    // it; that read must see a fresh overlay rather than the first trace's write.
    let sim_model = MockLlmClient::scripted(vec![
        workspace_call("write", json!({"path":"scratch", "content":"first trace"})),
        simulator_reply(),
        workspace_call("read", json!({"path":"scratch"})),
        simulator_reply(),
    ]);
    let put = workspace_put();
    let scenario = Scenario {
        world: "The simulator may use its workspace as private scratch.".into(),
        input_domain: Default::default(),
        user_message: None,
        simulator_notes: String::new(),
    };
    let runner = Runner::new(
        Arc::new(put_model),
        "put",
        None,
        Arc::new(sim_model),
        "sim",
        None,
        Workspace::empty(),
        RunnerOptions::default(),
    );

    let first = runner.run(&put, &scenario, &budget(), None).await.unwrap();
    let second = runner.run(&put, &scenario, &budget(), None).await.unwrap();

    assert_eq!(
        first.turns[0].tool_exchanges[0].workspace_ops[0].tool,
        "write"
    );
    let read = &second.turns[0].tool_exchanges[0].workspace_ops[0];
    assert_eq!(read.tool, "read");
    assert!(read.result.get("error").is_some());
}

fn workspace_put() -> PromptUnderTest {
    PromptUnderTest {
        id: "workspace".into(),
        template: "Use the probe tool.".into(),
        tools: vec![ToolSchema {
            name: "probe".into(),
            description: "Probe the simulated environment.".into(),
            parameters: json!({"type":"object"}),
            side_effect: SideEffect::Read,
            example_responses: vec![],
        }],
        design_goals: String::new(),
    }
}

fn tool_call(id: &str) -> ChatResponse {
    ChatResponse {
        content: None,
        thinking: None,
        tool_calls: vec![ToolCallRequest {
            id: id.into(),
            name: "probe".into(),
            arguments: "{}".into(),
        }],
        usage: None,
    }
}

fn final_reply() -> ChatResponse {
    ChatResponse {
        content: Some("done".into()),
        thinking: None,
        tool_calls: vec![],
        usage: None,
    }
}

fn workspace_call(name: &str, arguments: serde_json::Value) -> ChatResponse {
    ChatResponse {
        content: None,
        thinking: None,
        tool_calls: vec![ToolCallRequest {
            id: format!("workspace-{name}"),
            name: name.into(),
            arguments: arguments.to_string(),
        }],
        usage: None,
    }
}

fn simulator_reply() -> ChatResponse {
    ChatResponse {
        content: Some(r#"{"response":"ok"}"#.into()),
        thinking: None,
        tool_calls: vec![],
        usage: None,
    }
}

fn budget() -> Budget {
    Budget {
        max_steps_per_trace: 4,
        max_tokens: None,
    }
}
