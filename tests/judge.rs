//! Judge tests. The NL path is exercised via a scripted mock.

use std::sync::Arc;

use serde_json::json;

use prompt_explore::judge::Judge;
use prompt_explore::llm::{ChatResponse, MockLlmClient};
use prompt_explore::model::output::GoalFinding;
use prompt_explore::model::predicate::{Predicate, SuccessMode};
use prompt_explore::model::simulation::{Scenario, Trace, TraceStep, ToolCall};

fn trace_with_cancel() -> Trace {
    Trace {
        scenario_id: "s".into(),
        verdict: None,
        steps: vec![
            TraceStep {
                model_output: "ok".into(),
                tool_call: Some(ToolCall {
                    name: "order_status".into(),
                    args: json!({"order_id": "X"}),
                }),
                tool_response: Some(json!({"status": "processing"})),
                world_state_after: None,
            },
            TraceStep {
                model_output: "done".into(),
                tool_call: Some(ToolCall {
                    name: "cancel_order".into(),
                    args: json!({"order_id": "X"}),
                }),
                tool_response: Some(json!({"cancelled": true})),
                world_state_after: Some(
                    [("orders".to_string(), json!({"X": {"status": "cancelled"}}))]
                        .into_iter()
                        .collect(),
                ),
            },
        ],
    }
}

fn empty_scenario() -> Scenario {
    Scenario {
        id: "s".into(),
        hypothesis_id: "h".into(),
        put_id: "p".into(),
        resolved_inputs: Default::default(),
        user_message: Some("cancel it".into()),
        world_state: Default::default(),
        simulator_notes: "".into(),
    }
}

fn predicate(criterion: &str) -> Predicate {
    Predicate {
        criterion: criterion.into(),
        success_mode: SuccessMode::Witness,
    }
}

#[tokio::test]
async fn evaluate_uses_llm_judge_against_criterion() {
    let mock = MockLlmClient::scripted(vec![ChatResponse {
        content: Some(
            r#"{"matched": true, "confidence": 0.95,
                "rationale": "agent cancelled at step 1",
                "matched_step_indices": [1]}"#
                .into(),
        ),
        tool_calls: vec![],
        usage: None,
    }]);
    let judge = Judge::new(Arc::new(mock), "j");

    let verdict = judge
        .evaluate(
            &trace_with_cancel(),
            &predicate("the agent cancels an order"),
            Some(&empty_scenario()),
        )
        .await
        .unwrap();

    assert!(verdict.matched);
    assert_eq!(verdict.confidence, Some(0.95));
    assert_eq!(verdict.matched_step_indices, vec![1]);
}

#[tokio::test]
async fn check_goals_parses_findings() {
    let mock = MockLlmClient::scripted(vec![ChatResponse {
        content: Some(
            r#"{"findings": [{"goal": "confirm before cancelling", "violated": true,
                "rationale": "cancelled with no confirmation", "step_indices": [1]}]}"#
                .into(),
        ),
        tool_calls: vec![],
        usage: None,
    }]);
    let judge = Judge::new(Arc::new(mock), "j");

    let findings = judge
        .check_goals(&trace_with_cancel(), "confirm before cancelling", None)
        .await
        .unwrap();

    assert_eq!(findings.len(), 1);
    let g: &GoalFinding = &findings[0];
    assert!(g.violated);
    assert_eq!(g.step_indices, vec![1]);
}
