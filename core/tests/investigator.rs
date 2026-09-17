//! Singular investigator contract tests with scripted clients.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use prompt_explore::generate::{Investigator, LlmRole};
use prompt_explore::llm::{
    ChatRequest, ChatResponse, LlmClient, LlmError, MockLlmClient, ToolCallRequest,
};
use prompt_explore::model::*;
use prompt_explore::simulate::{RunnerOptions, Workspace};
use serde_json::json;

fn investigation() -> Investigation {
    Investigation {
        reason: None,
        budget: Budget {
            max_steps_per_trace: 4,
            max_tokens: None,
        },
    }
}

fn put() -> PromptUnderTest {
    PromptUnderTest {
        id: "test".into(),
        template: "Answer the user.".into(),
        tools: vec![],
        design_goals: String::new(),
    }
}

fn probe_put() -> PromptUnderTest {
    PromptUnderTest {
        id: "test".into(),
        template: "Answer the user about {{item}}.".into(),
        tools: vec![ToolSchema {
            name: "probe".into(),
            description: "Probe the simulated world.".into(),
            parameters: json!({"type": "object"}),
            side_effect: SideEffect::Read,
            example_responses: vec![],
        }],
        design_goals: String::new(),
    }
}

fn scenario() -> Scenario {
    Scenario {
        world: "The world is deliberately small.".into(),
        input_domain: HashMap::new(),
        user_message: Some("Hello".into()),
        simulator_notes: String::new(),
    }
}

fn investigator(put_client: Arc<dyn LlmClient>, sim_client: Arc<dyn LlmClient>) -> Investigator {
    Investigator {
        runner_put: LlmRole {
            client: put_client,
            model: "put".into(),
            thinking_level: None,
        },
        runner_sim: LlmRole {
            client: sim_client,
            model: "sim".into(),
            thinking_level: None,
        },
        workspace_seed: Workspace::empty(),
        runner_options: RunnerOptions::default(),
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

#[tokio::test]
async fn investigate_returns_one_trace_and_flat_live_progress() {
    let put_client = Arc::new(MockLlmClient::scripted(vec![ChatResponse {
        content: Some("Done.".into()),
        thinking: None,
        tool_calls: vec![],
        usage: None,
    }]));
    let scenario = scenario();
    let progress = Arc::new(Mutex::new(RunProgress::default()));

    let outcome = investigator(put_client, Arc::new(MockLlmClient::scripted(vec![])))
        .investigate(&investigation(), &put(), &scenario, Some(progress.clone()))
        .await;

    assert_eq!(outcome.scenario.world, scenario.world);
    assert!(outcome.failure.is_none());
    assert_eq!(outcome.trace.unwrap().turns.len(), 1);
    let progress = progress.lock().unwrap();
    assert_eq!(progress.phase, RunPhase::PutLoop);
    assert_eq!(progress.user_message.as_deref(), Some("Hello"));
    assert_eq!(progress.turns.len(), 1);
}

#[tokio::test]
async fn investigate_failure_keeps_inputs_and_completed_turns() {
    let put_client = Arc::new(MockLlmClient::scripted(vec![
        tool_call("one"),
        tool_call("two"),
        // The third PUT completion fails after both tool exchanges have been
        // recorded in flat progress.
    ]));
    let sim_client = Arc::new(MockLlmClient::scripted(vec![
        ChatResponse {
            content: Some(r#"{"item":"A-123"}"#.into()),
            thinking: None,
            tool_calls: vec![],
            usage: None,
        },
        ChatResponse {
            content: Some(r#"{"response":"first"}"#.into()),
            thinking: None,
            tool_calls: vec![],
            usage: None,
        },
        ChatResponse {
            content: Some(r#"{"response":"second"}"#.into()),
            thinking: None,
            tool_calls: vec![],
            usage: None,
        },
    ]));
    let mut scenario = scenario();
    scenario
        .input_domain
        .insert("item".into(), "the only item id".into());
    let progress = Arc::new(Mutex::new(RunProgress::default()));

    let outcome = investigator(put_client, sim_client)
        .investigate(
            &investigation(),
            &probe_put(),
            &scenario,
            Some(progress.clone()),
        )
        .await;

    assert!(outcome.trace.is_none());
    let failure = outcome.failure.expect("exactly one failure");
    assert_eq!(failure.stage, "runner");
    assert!(failure.error.contains("mock script exhausted"));
    let progress = progress.lock().unwrap();
    assert_eq!(progress.phase, RunPhase::PutLoop);
    assert_eq!(progress.user_message.as_deref(), Some("Hello"));
    assert_eq!(progress.resolved_inputs["item"], "A-123");
    assert_eq!(progress.turns.len(), 2);
    assert_eq!(progress.turns[0].tool_exchanges[0].response, "first");
    assert_eq!(progress.turns[1].tool_exchanges[0].response, "second");
}

struct PanicClient;

#[async_trait::async_trait]
impl LlmClient for PanicClient {
    async fn complete(&self, _: ChatRequest) -> Result<ChatResponse, LlmError> {
        panic!("deliberate PUT panic")
    }
}

#[tokio::test]
async fn investigate_captures_runner_task_panics_as_failure_evidence() {
    let scenario = scenario();
    let progress = Arc::new(Mutex::new(RunProgress::default()));

    let outcome = investigator(
        Arc::new(PanicClient),
        Arc::new(MockLlmClient::scripted(vec![])),
    )
    .investigate(&investigation(), &put(), &scenario, Some(progress.clone()))
    .await;

    assert_eq!(outcome.scenario.world, scenario.world);
    assert!(outcome.trace.is_none());
    let failure = outcome.failure.expect("panic becomes one failure");
    assert_eq!(failure.stage, "runner");
    assert!(failure.error.contains("task panicked"));
    assert!(failure.error.contains("deliberate PUT panic"));
    let progress = progress.lock().unwrap();
    assert_eq!(progress.phase, RunPhase::PutLoop);
    assert_eq!(progress.user_message.as_deref(), Some("Hello"));
}
