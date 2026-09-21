use prompt_explore::llm::{
    ChatRequest, ChatResponse, LlmClient, MockLlmClient, PricingMap, Usage, UsageTracker,
};
use std::sync::Arc;

/// HTTP clients own background tasks (connection pool drivers). All workflow
/// calls must stay on the caller's runtime so ending one investigation cannot
/// shut down a runtime that still owns another investigation's connection.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_workflows_keep_the_callers_runtime_and_default_params() {
    use prompt_explore::generate::{Investigator, LlmRole};
    use prompt_explore::llm::{LlmError, Message};
    use prompt_explore::model::{
        Budget, Investigation, PromptUnderTest, Scenario, WorkflowProgram,
    };
    use prompt_explore::simulate::{RunnerOptions, ScenarioRuntime, Workspace};
    struct RuntimeClient(tokio::runtime::Id);
    #[async_trait::async_trait]
    impl LlmClient for RuntimeClient {
        async fn complete(&self, request: ChatRequest) -> Result<ChatResponse, LlmError> {
            assert_eq!(tokio::runtime::Handle::current().id(), self.0);
            assert_eq!(request.model, "test::runtime");
            assert!(
                matches!(&request.messages[0], Message::System {content} if content == "literal prompt")
            );
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            Ok(ChatResponse {
                content: Some("ok".into()),
                thinking: None,
                tool_calls: vec![],
                usage: None,
            })
        }
    }
    let role = LlmRole {
        client: Arc::new(RuntimeClient(tokio::runtime::Handle::current().id())),
        model: "test::runtime".into(),
        thinking_level: None,
    };
    let investigator = Investigator {
        runner_put: role.clone(),
        runner_sim: role,
        runner_options: RunnerOptions::default(),
    };
    let put = PromptUnderTest {
        id: "runtime-test".into(),
        template: "literal prompt".into(),
        tools: vec![],
        design_goals: String::new(),
    };
    let scenario = Scenario {
        world: "No tools or other facts.".into(),
        input_domain: Default::default(),
        user_message: Some("hello".into()),
        simulator_notes: String::new(),
    };
    let runtime = ScenarioRuntime::from_put(&put, scenario, Workspace::empty());
    let investigation = Investigation {
        reason: None,
        budget: Budget {
            max_steps_per_trace: 1,
            max_tokens: None,
        },
    };
    let workflow = WorkflowProgram {
        params: serde_json::json!({"prompt":"literal prompt","model":"test::runtime"}),
        limits: prompt_explore::model::WorkflowLimits {
            max_host_calls: 17,
            ..Default::default()
        },
        ..Default::default()
    };
    let (legacy, custom) = tokio::join!(
        investigator.investigate(&investigation, &put, &runtime, None, None),
        investigator.investigate_workflow(&investigation, &put, &runtime, None, None, &workflow)
    );
    assert!(legacy.failure.is_none(), "{:?}", legacy.failure);
    assert!(custom.failure.is_none(), "{:?}", custom.failure);
    for (result, expected_host_cap) in [legacy, custom].into_iter().zip([128, 17]) {
        let trace = result.trace.unwrap();
        assert_eq!(
            trace.execution.stop_reason,
            Some(prompt_explore::model::RunStopReason::FinalCompletion)
        );
        let evidence = trace.workflow.unwrap();
        assert_eq!(evidence.output, Some(serde_json::json!("ok")));
        assert_eq!(evidence.limits.unwrap().max_host_calls, expected_host_cap);
    }
}

#[tokio::test]
async fn default_program_does_not_silently_swallow_agent_failure() {
    use prompt_explore::generate::{Investigator, LlmRole};
    use prompt_explore::model::{
        Budget, Investigation, PromptUnderTest, Scenario, WorkflowProgram,
    };
    use prompt_explore::simulate::{RunnerOptions, ScenarioRuntime, Workspace};
    let role = LlmRole {
        client: Arc::new(MockLlmClient::scripted(vec![])),
        model: "test::unavailable".into(),
        thinking_level: None,
    };
    let investigator = Investigator {
        runner_put: role.clone(),
        runner_sim: role,
        runner_options: RunnerOptions::default(),
    };
    let put = PromptUnderTest {
        id: "unused".into(),
        template: "unused".into(),
        tools: vec![],
        design_goals: String::new(),
    };
    let runtime = ScenarioRuntime::from_put(
        &put,
        Scenario {
            world: "empty".into(),
            input_domain: Default::default(),
            user_message: None,
            simulator_notes: String::new(),
        },
        Workspace::empty(),
    );
    let investigation = Investigation {
        reason: None,
        budget: Budget {
            max_steps_per_trace: 3,
            max_tokens: None,
        },
    };
    let workflow = WorkflowProgram {
        params: serde_json::json!({"prompt":"hello","model":"test::unavailable"}),
        ..Default::default()
    };
    let progress = Arc::new(std::sync::Mutex::new(
        prompt_explore::model::RunProgress::default(),
    ));
    let result = investigator
        .investigate_workflow(
            &investigation,
            &put,
            &runtime,
            None,
            Some(progress.clone()),
            &workflow,
        )
        .await;
    assert!(result.trace.is_none());
    assert!(
        result
            .failure
            .unwrap()
            .error
            .contains("mock script exhausted")
    );
    assert!(
        progress
            .lock()
            .unwrap()
            .workflow
            .as_ref()
            .unwrap()
            .invocations[0]
            .failure
            .is_some()
    );
}

#[tokio::test]
async fn mixed_models_are_priced_separately_and_unknown_prices_are_not_zero() {
    let response = ChatResponse {
        content: Some("ok".into()),
        thinking: None,
        tool_calls: vec![],
        usage: Some(Usage {
            input_tokens: 100,
            output_tokens: 10,
            cache_read_tokens: 0,
        }),
    };
    let tracker = UsageTracker::new(Arc::new(MockLlmClient::scripted(vec![
        response.clone(),
        response,
    ])));
    for model in ["cheap", "dear"] {
        tracker
            .complete(ChatRequest {
                model: model.into(),
                messages: vec![],
                tools: vec![],
                temperature: None,
                max_tokens: None,
                thinking_level: None,
            })
            .await
            .unwrap();
    }
    assert_eq!(tracker.totals().input_tokens, 200);
    assert_eq!(tracker.by_model()["cheap"].llm_calls, 1);
    assert_eq!(tracker.by_model()["dear"].output_tokens, 10);
    let mut prices = PricingMap::new();
    prices.insert(
        "cheap".into(),
        [
            ("prompt".into(), "0.001".into()),
            ("completion".into(), "0.002".into()),
        ]
        .into(),
    );
    assert!(tracker.priced_totals(&prices).cost_usd.is_none());
    prices.insert(
        "dear".into(),
        [
            ("prompt".into(), "0.01".into()),
            ("completion".into(), "0.02".into()),
        ]
        .into(),
    );
    assert!((tracker.priced_totals(&prices).cost_usd.unwrap() - 1.32).abs() < 1e-12);
}
