//! End-to-end runner test with scripted PUT-model and simulator
//! responses: no network, fully deterministic.

use std::sync::{Arc, Mutex};

use serde_json::json;

use prompt_explore::llm::{ChatResponse, MockLlmClient, ToolCallRequest};
use prompt_explore::model::*;
use prompt_explore::simulate::{Runner, RunnerOptions, Workspace};

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
            thinking: None,
            tool_calls: vec![ToolCallRequest {
                id: "call_1".into(),
                name: "cancel_order".into(),
                arguments: r#"{"order_id":"A-1234"}"#.into(),
            }],
            usage: None,
        },
        ChatResponse {
            content: Some("Done — order cancelled.".into()),
            thinking: None,
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
        thinking: None,
        tool_calls: vec![],
        usage: None,
    }]);

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
    let trace = runner
        .run(&support_put(), &scenario(), &budget(), None)
        .await
        .unwrap();

    assert_eq!(trace.turns.len(), 2);

    let exchange = &trace.turns[0].tool_exchanges[0];
    assert_eq!(exchange.call.name, "cancel_order");
    assert_eq!(exchange.response["cancelled"], json!(true));
    // A write-tool exchange records the resulting world state.
    assert_eq!(
        exchange.world_state_after.as_ref().unwrap()["orders"]["A-1234"]["status"],
        json!("cancelled")
    );

    assert!(trace.turns[1].tool_exchanges.is_empty());
}

#[tokio::test]
async fn invalid_arguments_are_fed_back_without_simulator_call() {
    let put_model = MockLlmClient::scripted(vec![
        ChatResponse {
            content: None,
            thinking: None,
            tool_calls: vec![ToolCallRequest {
                id: "call_1".into(),
                name: "cancel_order".into(),
                arguments: r#"{"order_id":42}"#.into(), // wrong type
            }],
            usage: None,
        },
        ChatResponse {
            content: Some("sorry, I need the order id as text".into()),
            thinking: None,
            tool_calls: vec![],
            usage: None,
        },
    ]);
    // No scripted simulator responses: the runner must never call it.
    let sim_model = MockLlmClient::scripted(vec![]);

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
    let trace = runner
        .run(&support_put(), &scenario(), &budget(), None)
        .await
        .unwrap();

    let resp = &trace.turns[0].tool_exchanges[0].response;
    assert!(resp.as_str().unwrap().contains("invalid arguments"));
}

#[tokio::test]
async fn runner_options_reach_put_requests() {
    let put = PromptUnderTest {
        tools: vec![],
        ..support_put()
    };
    let put_model = Arc::new(MockLlmClient::scripted(vec![ChatResponse {
        content: Some("done".into()),
        thinking: None,
        tool_calls: vec![],
        usage: None,
    }]));
    let options = RunnerOptions {
        put_temperature: Some(0.25),
        put_max_tokens: Some(1234),
        ..RunnerOptions::default()
    };
    let runner = Runner::new(
        put_model.clone(),
        "put-model",
        None,
        Arc::new(MockLlmClient::scripted(vec![])),
        "sim-model",
        None,
        Workspace::empty(),
        options,
    );

    runner
        .run(&put, &scenario(), &budget(), None)
        .await
        .unwrap();

    let requests = put_model.requests.lock().unwrap();
    assert_eq!(requests[0].temperature, Some(0.25));
    assert_eq!(requests[0].max_tokens, Some(1234));
}

#[tokio::test]
async fn empty_tool_array_means_single_shot() {
    let mut put = support_put();
    put.tools = vec![];

    let put_model = MockLlmClient::scripted(vec![ChatResponse {
        content: Some("I can help with that.".into()),
        thinking: None,
        tool_calls: vec![],
        usage: None,
    }]);
    let sim_model = MockLlmClient::scripted(vec![]);

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
    let trace = runner
        .run(&put, &scenario(), &budget(), None)
        .await
        .unwrap();

    assert_eq!(trace.turns.len(), 1);
    assert!(trace.turns[0].tool_exchanges.is_empty());
}

#[tokio::test]
async fn failed_sibling_keeps_completed_exchanges_in_one_progress_turn() {
    let put_model = MockLlmClient::scripted(vec![ChatResponse {
        content: Some("I will cancel both orders.".into()),
        thinking: Some("Both cancellations are requested together.".into()),
        tool_calls: vec![
            ToolCallRequest {
                id: "call_1".into(),
                name: "cancel_order".into(),
                arguments: r#"{"order_id":"A-1234"}"#.into(),
            },
            ToolCallRequest {
                id: "call_2".into(),
                name: "cancel_order".into(),
                arguments: r#"{"order_id":"B-5678"}"#.into(),
            },
        ],
        usage: None,
    }]);
    // The first simulation consults its workspace and succeeds with a write
    // patch. The second simulator call exhausts this script and fails.
    let sim_model = MockLlmClient::scripted(vec![
        ChatResponse {
            content: None,
            thinking: None,
            tool_calls: vec![ToolCallRequest {
                id: "workspace_1".into(),
                name: "write".into(),
                arguments: r#"{"path":"audit","content":"first cancellation"}"#.into(),
            }],
            usage: None,
        },
        ChatResponse {
            content: Some(
                r#"{"response":{"cancelled":"A-1234"},"state_patch":{"orders":{"A-1234":{"status":"cancelled"}}}}"#.into(),
            ),
            thinking: None,
            tool_calls: vec![],
            usage: None,
        },
    ]);
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
    let progress = Arc::new(Mutex::new(RunProgress::default()));

    let error = runner
        .run(
            &support_put(),
            &scenario(),
            &budget(),
            Some(progress.clone()),
        )
        .await
        .expect_err("the second simulator call fails");

    assert!(error.to_string().contains("mock script exhausted"));
    let progress = progress.lock().unwrap();
    assert_eq!(
        progress.turns.len(),
        1,
        "one model completion stays one turn"
    );
    let turn = &progress.turns[0];
    assert_eq!(turn.model_output, "I will cancel both orders.");
    assert_eq!(
        turn.thinking.as_deref(),
        Some("Both cancellations are requested together.")
    );
    assert_eq!(
        turn.tool_exchanges.len(),
        1,
        "no failed exchange is invented"
    );
    let exchange = &turn.tool_exchanges[0];
    assert_eq!(exchange.call.args["order_id"], "A-1234");
    assert_eq!(exchange.response["cancelled"], "A-1234");
    assert_eq!(
        exchange.world_state_after.as_ref().unwrap()["orders"]["A-1234"]["status"],
        "cancelled"
    );
    assert_eq!(exchange.workspace_ops[0].tool, "write");
}

#[tokio::test]
async fn multi_tool_completion_is_one_atomic_turn() {
    let put_model = Arc::new(MockLlmClient::scripted(vec![ChatResponse {
        content: Some("I will check both requests together.".into()),
        thinking: Some("Two independent lookups are needed.".into()),
        tool_calls: vec![
            ToolCallRequest {
                id: "call_1".into(),
                name: "cancel_order".into(),
                arguments: r#"{"order_id":"A-1234"}"#.into(),
            },
            ToolCallRequest {
                id: "call_2".into(),
                name: "cancel_order".into(),
                arguments: r#"{"order_id":"B-5678"}"#.into(),
            },
        ],
        usage: None,
    }]));
    let sim_model = MockLlmClient::scripted(vec![
        ChatResponse {
            content: Some(r#"{"response":{"cancelled":"A-1234"},"state_patch":{}}"#.into()),
            thinking: None,
            tool_calls: vec![],
            usage: None,
        },
        ChatResponse {
            content: Some(r#"{"response":{"cancelled":"B-5678"},"state_patch":{}}"#.into()),
            thinking: None,
            tool_calls: vec![],
            usage: None,
        },
    ]);
    let runner = Runner::new(
        put_model.clone(),
        "put-model",
        None,
        Arc::new(sim_model),
        "sim-model",
        None,
        Workspace::empty(),
        RunnerOptions::default(),
    );
    let trace = runner
        .run(
            &support_put(),
            &scenario(),
            &Budget {
                max_steps_per_trace: 1,
                max_tokens: None,
            },
            None,
        )
        .await
        .unwrap();

    assert_eq!(trace.turns.len(), 1);
    assert_eq!(trace.turns[0].tool_exchanges.len(), 2);
    assert_eq!(
        trace.turns[0].model_output,
        "I will check both requests together."
    );
    assert_eq!(trace.step_count(), 2, "the whole batch may cross the cap");
    assert_eq!(trace.tool_call_count(), 2);
    assert_eq!(put_model.requests.lock().unwrap().len(), 1);

    let value = serde_json::to_value(&trace).unwrap();
    assert!(value.get("steps").is_none());
    assert_eq!(
        value["turns"][0]["tool_exchanges"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        value["turns"][0]["tool_exchanges"][1]["call"]["args"]["order_id"],
        "B-5678"
    );
}
