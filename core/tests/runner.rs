//! End-to-end runner test with scripted PUT-model and simulator
//! responses: no network, fully deterministic.

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::json;

use prompt_explore::llm::{ChatResponse, MockLlmClient, ToolCallRequest};
use prompt_explore::model::*;
use prompt_explore::simulate::Runner;

fn support_put() -> PromptUnderTest {
    PromptUnderTest {
        id: "support".into(),
        template: "You are a support agent.".into(),
        tools: vec![ToolSchema {
            name: "cancel_order".into(),
            description: "Cancel an order.".into(),
            parameters: json!({
                "type": "object",
                "properties": { "order_id": { "type": "string" } },
                "required": ["order_id"]
            }),
            side_effect: SideEffect::Write,
            example_responses: vec![],
        }],
        design_goals: "always confirm before cancelling".into(),
    }
}

fn scenario() -> Scenario {
    Scenario {
        world: "One order A-1234 (status: shipped). cancel_order cancels by id.".into(),
        input_domain: Default::default(),
        user_message: Some("cancel my order A-1234!".into()),
        simulator_notes: "customer is angry".into(),
    }
}

fn budget() -> Budget {
    Budget {
        max_steps_per_trace: 10,
        max_tokens: None,
    }
}

#[tokio::test]
async fn tool_call_loop_runs_and_mutates_state() {
    // PUT model: calls cancel_order, then stops with a text reply.
    let put_model = MockLlmClient::scripted(vec![
        ChatResponse {
            content: None,
            tool_calls: vec![ToolCallRequest {
                id: "call_1".into(),
                name: "cancel_order".into(),
                arguments: r#"{"order_id":"A-1234"}"#.into(),
            }],
            usage: None,
        },
        ChatResponse {
            content: Some("Done — order cancelled.".into()),
            tool_calls: vec![],
            usage: None,
        },
    ]);
    // Simulator: confirms cancellation with a state patch.
    let sim_model = MockLlmClient::scripted(vec![ChatResponse {
        content: Some(
            r#"{"response": {"cancelled": true},
                "state_patch": {"orders": {"A-1234": {"status": "cancelled"}}}}"#
                .into(),
        ),
        tool_calls: vec![],
        usage: None,
    }]);

    let runner = Runner::new(
        Arc::new(put_model),
        "put-model",
        Arc::new(sim_model),
        "sim-model",
    );
    let trace = runner
        .run(&support_put(), &scenario(), &budget(), 0, None)
        .await
        .unwrap();

    assert_eq!(trace.steps.len(), 2);

    let call_step = &trace.steps[0];
    assert_eq!(call_step.tool_call.as_ref().unwrap().name, "cancel_order");
    assert_eq!(
        call_step.tool_response.as_ref().unwrap()["cancelled"],
        json!(true)
    );
    // Write-tool step records the resulting world state.
    assert_eq!(
        call_step.world_state_after.as_ref().unwrap()["orders"]["A-1234"]["status"],
        json!("cancelled")
    );

    assert!(trace.steps[1].tool_call.is_none());
    assert!(trace.verdict.is_none());
}

#[tokio::test]
async fn invalid_arguments_are_fed_back_without_simulator_call() {
    let put_model = MockLlmClient::scripted(vec![
        ChatResponse {
            content: None,
            tool_calls: vec![ToolCallRequest {
                id: "call_1".into(),
                name: "cancel_order".into(),
                arguments: r#"{"order_id":42}"#.into(), // wrong type
            }],
            usage: None,
        },
        ChatResponse {
            content: Some("sorry, I need the order id as text".into()),
            tool_calls: vec![],
            usage: None,
        },
    ]);
    // No scripted simulator responses: the runner must never call it.
    let sim_model = MockLlmClient::scripted(vec![]);

    let runner = Runner::new(
        Arc::new(put_model),
        "put-model",
        Arc::new(sim_model),
        "sim-model",
    );
    let trace = runner
        .run(&support_put(), &scenario(), &budget(), 0, None)
        .await
        .unwrap();

    let resp = trace.steps[0].tool_response.as_ref().unwrap();
    assert!(resp.as_str().unwrap().contains("invalid arguments"));
}

#[tokio::test]
async fn empty_tool_array_means_single_shot() {
    let mut put = support_put();
    put.tools = vec![];

    let put_model = MockLlmClient::scripted(vec![ChatResponse {
        content: Some("I can help with that.".into()),
        tool_calls: vec![],
        usage: None,
    }]);
    let sim_model = MockLlmClient::scripted(vec![]);

    let runner = Runner::new(
        Arc::new(put_model),
        "put-model",
        Arc::new(sim_model),
        "sim-model",
    );
    let trace = runner
        .run(&put, &scenario(), &budget(), 0, None)
        .await
        .unwrap();

    assert_eq!(trace.steps.len(), 1);
}
