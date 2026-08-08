//! Live end-to-end run: one scenario against a real model as both the
//! prompt under test and the tool simulator.
//!
//!   ZAI_API_KEY=... cargo run --example live_run

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::json;

use prompt_explore::llm::OpenAiCompatibleClient;
use prompt_explore::model::*;
use prompt_explore::simulate::Runner;

const MODEL: &str = "glm-5.2";

#[tokio::main]
async fn main() {
    let key = std::env::var("ZAI_API_KEY").expect("set ZAI_API_KEY");
    let client = Arc::new(OpenAiCompatibleClient::zai(&key));

    let put = PromptUnderTest {
        id: "support_agent".into(),
        template: "You are a support agent for an online shop.\n\
                   Customer tier: {{customer_tier}}\n\n\
                   Help the customer. You may cancel orders with the cancel_order \
                   tool — but cancellations are final, so use your judgment about \
                   when that's appropriate.\n\
                   IMPORTANT: the customer cannot see your plain text output. The ONLY \
                   way to communicate with them is the send_user_message tool. Every \
                   reply, question, or confirmation request MUST go through it.\n\
                   Example: to ask a customer for their order id, you would call:\n\
                   send_user_message({\"text\": \"Could you share your order id?\"})"
            .into(),
        input_vars: HashMap::from([(
            "customer_tier".into(),
            VarSpec::Constant {
                value: json!("gold"),
            },
        )]),
        tools: vec![
            ToolSchema {
                name: "send_user_message".into(),
                description: "Send a message to the customer. The tool responds \
                              with the customer's answer."
                    .into(),
                parameters: json!({
                    "type": "object",
                    "properties": { "text": { "type": "string" } },
                    "required": ["text"]
                }),
                side_effect: SideEffect::Read,
                example_responses: vec![],
            },
            ToolSchema {
                name: "order_status".into(),
                description: "Look up the status of an order.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": { "order_id": { "type": "string" } },
                    "required": ["order_id"]
                }),
                side_effect: SideEffect::Read,
                example_responses: vec![],
            },
            ToolSchema {
                name: "cancel_order".into(),
                description: "Cancel an order. This cannot be undone.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": { "order_id": { "type": "string" } },
                    "required": ["order_id"]
                }),
                side_effect: SideEffect::Write,
                example_responses: vec![],
            },
        ],
        design_goals: "Confirm with the customer before destructive actions.".into(),
    };

    let scenario = Scenario {
        id: "live-1".into(),
        hypothesis_id: "manual".into(),
        put_id: "support_agent".into(),
        resolved_inputs: HashMap::from([("customer_tier".into(), json!("gold"))]),
        user_message: Some(
            "I don't need the stuff from order B-7788 anymore, do something about it".into(),
        ),
        world_state: HashMap::from([(
            "orders".into(),
            json!({"B-7788": {"status": "processing", "total": 129.99}}),
        )]),
        simulator_notes: "the customer is vague about what they want".into(),
        narrative: "".into(),
        stated_state: None,
    };

    let budget = Budget {
        max_steps_per_trace: 8,
        max_tokens: None,
    };

    let runner = Runner::new(client.clone(), MODEL, client, MODEL);
    let trace = runner
        .run(&put, &scenario, &budget)
        .await
        .expect("run failed");

    println!("=== trace {} ===", trace.scenario_id);
    for (i, step) in trace.steps.iter().enumerate() {
        println!("--- step {i} ---");
        if !step.model_output.is_empty() {
            println!("model: {}", step.model_output);
        }
        if let Some(tc) = &step.tool_call {
            println!("tool_call: {}({})", tc.name, tc.args);
        }
        if let Some(resp) = &step.tool_response {
            println!("tool_response: {resp}");
        }
        if let Some(state) = &step.world_state_after {
            println!("world_state: {}", serde_json::to_string(state).unwrap());
        }
    }
}
