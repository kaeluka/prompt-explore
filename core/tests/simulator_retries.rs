//! Malformed simulator replies are repaired in-place, not by rerunning the
//! scenario or its workspace writes. All responses are deterministic scripts.
use std::collections::HashMap;
use std::sync::Arc;

use prompt_explore::llm::{ChatResponse, Message, MockLlmClient, ToolCallRequest};
use prompt_explore::model::simulation::ToolCall;
use prompt_explore::model::{SideEffect, ToolSchema};
use prompt_explore::simulate::{SimulatorOptions, ToolSimulator, Workspace};
use serde_json::json;

fn reply(content: Option<&str>) -> ChatResponse {
    ChatResponse {
        content: content.map(str::to_owned),
        thinking: None,
        tool_calls: vec![],
        usage: None,
    }
}

fn simulator(client: Arc<MockLlmClient>, attempts: usize) -> ToolSimulator {
    ToolSimulator::new(
        client,
        "scripted-sim",
        None,
        SimulatorOptions {
            max_repair_attempts: attempts,
            ..Default::default()
        },
    )
}

fn tool() -> ToolSchema {
    ToolSchema {
        name: "read_file".into(),
        description: "Read a file".into(),
        parameters: json!({"type":"object"}),
        side_effect: SideEffect::Read,
        example_responses: vec![],
    }
}

fn call() -> ToolCall {
    ToolCall {
        name: "read_file".into(),
        args: json!({"path":"hello.py"}),
    }
}

#[tokio::test]
async fn default_budget_survives_nineteen_bad_replies_and_preserves_repair_feedback() {
    let attempts = SimulatorOptions::default().max_repair_attempts;
    assert_eq!(attempts, 20);
    let mut responses = vec![reply(Some(r#"{"response":"print("hello")"}"#)); attempts - 1];
    responses.push(reply(Some(r#"{"response":"print(\"hello\")\n"}"#)));
    let client = Arc::new(MockLlmClient::scripted(responses));
    let mut session = simulator(client.clone(), attempts).session(
        "hello.py contains print(\"hello\").",
        &Workspace::empty(),
        &[],
    );
    let outcome = session
        .respond(&tool(), &call(), &Default::default())
        .await
        .unwrap();
    assert_eq!(outcome.response, "print(\"hello\")\n");
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests.len(), 20);
    // The request isn't restarted: the initial system/world and user call
    // remain, and each malformed reply is followed by diagnostic feedback.
    assert_eq!(
        requests[19]
            .messages
            .iter()
            .filter(|m| matches!(m, Message::User { .. }))
            .count(),
        1
    );
    assert!(
        requests[1].messages.iter().any(
            |m| matches!(m, Message::Assistant { content: Some(c), .. } if c.contains("print("))
        )
    );
    assert!(requests[1].messages.iter().any(|m| matches!(m, Message::System { content } if content.contains("line 1 column") && content.contains("expected `,` or `}`"))));
}

#[tokio::test]
async fn override_is_an_exact_total_attempt_limit_and_error_keeps_raw_reply() {
    let raw = r#"{"response":"unterminated"#;
    let client = Arc::new(MockLlmClient::scripted(vec![reply(Some(raw)); 5]));
    let mut session = simulator(client.clone(), 3).session("one file", &Workspace::empty(), &[]);
    let error = session
        .respond(&tool(), &call(), &Default::default())
        .await
        .err()
        .unwrap()
        .to_string();
    assert_eq!(client.requests.lock().unwrap().len(), 3);
    assert!(error.contains("after 3 attempts"));
    assert!(error.contains("EOF while parsing a string"));
    assert!(error.ends_with(raw));
}

#[tokio::test]
async fn empty_final_reply_does_not_report_an_earlier_raw_reply() {
    let client = Arc::new(MockLlmClient::scripted(vec![
        reply(Some("earlier malformed reply")),
        reply(None),
    ]));
    let mut session = simulator(client.clone(), 2).session("one file", &Workspace::empty(), &[]);
    let error = session
        .respond(&tool(), &call(), &Default::default())
        .await
        .err()
        .unwrap()
        .to_string();
    assert!(error.contains("reply was empty"));
    assert!(!error.contains("earlier malformed reply"));
    assert_eq!(client.requests.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn empty_replies_and_schema_errors_recover_and_budget_resets_per_response() {
    let client = Arc::new(MockLlmClient::scripted(vec![
        reply(None),
        reply(Some(r#"{"response":"first"}"#)),
        reply(Some(r#"{"response":"wrong patch", "state_patch":42}"#)),
        reply(Some(r#"{"response":"second"}"#)),
    ]));
    let mut session = simulator(client.clone(), 2).session("one file", &Workspace::empty(), &[]);
    assert_eq!(
        session
            .respond(&tool(), &call(), &Default::default())
            .await
            .unwrap()
            .response,
        "first"
    );
    assert_eq!(
        session
            .respond(&tool(), &call(), &Default::default())
            .await
            .unwrap()
            .response,
        "second"
    );
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests.len(), 4);
    assert!(
        requests[1].messages.iter().any(
            |m| matches!(m, Message::System { content } if content.contains("reply was empty"))
        )
    );
    assert!(requests[3].messages.iter().any(|m| matches!(m, Message::System { content } if content.contains("invalid type: integer `42`, expected a map"))));
}

#[tokio::test]
async fn resolution_uses_the_same_repair_budget() {
    let mut responses = vec![reply(None); 6];
    responses.push(reply(Some(r#"{"path":"hello.py"}"#)));
    let client = Arc::new(MockLlmClient::scripted(responses));
    let mut session =
        simulator(client.clone(), 20).session("Only hello.py exists.", &Workspace::empty(), &[]);
    let values = session
        .resolve_domain(
            &HashMap::from([("path".into(), "The sole file's path".into())]),
            None,
        )
        .await
        .unwrap();
    assert_eq!(values["path"], "hello.py");
    assert_eq!(client.requests.lock().unwrap().len(), 7);
}

#[tokio::test]
async fn repair_does_not_replay_workspace_operations() {
    let client = Arc::new(MockLlmClient::scripted(vec![
        ChatResponse {
            content: None,
            thinking: None,
            tool_calls: vec![ToolCallRequest {
                id: "w1".into(),
                name: "write".into(),
                arguments: r#"{"path":"scratch.txt","content":"saved once"}"#.into(),
            }],
            usage: None,
        },
        reply(Some("invalid JSON")),
        reply(Some(r#"{"response":"done"}"#)),
    ]));
    let mut session = simulator(client.clone(), 2).session("one file", &Workspace::empty(), &[]);
    let outcome = session
        .respond(&tool(), &call(), &Default::default())
        .await
        .unwrap();
    assert_eq!(outcome.response, "done");
    assert_eq!(outcome.workspace_ops.len(), 1);
    assert_eq!(outcome.workspace_ops[0].tool, "write");
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    assert_eq!(
        requests[2]
            .messages
            .iter()
            .filter(|m| matches!(m, Message::Tool { tool_call_id, .. } if tool_call_id == "w1"))
            .count(),
        1
    );
}
