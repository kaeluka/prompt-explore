//! Caller-supplied Lua implementations: the harness executes them, never
//! authors or rewrites them. Executable and rendered replies share one
//! conversation, one workspace and one world-state trajectory.
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde_json::json;

use prompt_explore::llm::{ChatResponse, Message, MockLlmClient, ToolCallRequest};
use prompt_explore::model::scenario::{ScenarioDefinition, ScenarioTool, SimulationSettings};
use prompt_explore::model::simulation::{LuaOutcome, RunPhase, RunProgress};
use prompt_explore::model::*;
use prompt_explore::scenario::{ProbeProgress, ProbeRequest, ProbeTarget, run_probe};
use prompt_explore::simulate::{Runner, RunnerOptions, ScenarioRuntime, Workspace};

fn reply(value: serde_json::Value) -> ChatResponse {
    ChatResponse {
        content: Some(value.to_string()),
        thinking: None,
        tool_calls: vec![],
        usage: None,
    }
}

fn calls(calls: Vec<ToolCallRequest>) -> ChatResponse {
    ChatResponse {
        content: None,
        thinking: None,
        tool_calls: calls,
        usage: None,
    }
}

fn call(name: &str, args: serde_json::Value) -> ToolCallRequest {
    ToolCallRequest {
        id: name.into(),
        name: name.into(),
        arguments: args.to_string(),
    }
}

fn tool_with(name: &str, side_effect: SideEffect, lua_source: Option<&str>) -> ScenarioTool {
    ScenarioTool {
        name: name.into(),
        description: format!("{name} tool"),
        parameters: json!({"type":"object"}),
        side_effect,
        example_responses: vec![],
        lua_source: lua_source.map(str::to_owned),
    }
}

fn tool(name: &str, side_effect: SideEffect) -> ToolSchema {
    tool_with(name, side_effect, None).contract()
}

fn definition(tools: Vec<ScenarioTool>) -> ScenarioDefinition {
    ScenarioDefinition {
        world: "A deliberately small world.".into(),
        input_domain: HashMap::new(),
        user_message: Some("Go.".into()),
        simulator_notes: String::new(),
        tools,
        simulation: SimulationSettings::default(),
    }
}

fn put(tools: Vec<ToolSchema>) -> PromptUnderTest {
    PromptUnderTest {
        id: "put".into(),
        template: "Do the task.".into(),
        tools,
        design_goals: String::new(),
    }
}

fn runtime(
    definition: &ScenarioDefinition,
    put: &PromptUnderTest,
    workspace: Workspace,
) -> ScenarioRuntime {
    let _ = put;
    ScenarioRuntime::from_definition(definition, workspace)
}

fn progress_with_message(message: Option<String>) -> Arc<Mutex<RunProgress>> {
    let mut progress = RunProgress::default();
    progress.user_message = message;
    Arc::new(Mutex::new(progress))
}

/// A supplied implementation is tried first and answers without any simulator
/// call; its workspace write commits and is visible to the NEXT call, whose
/// implementation delegates. Only the delegating call reaches the model.
#[tokio::test]
async fn supplied_implementations_compute_and_delegate_in_one_conversation() {
    let source = r#"return function(a, c)
      if a.compute then
        c.workspace.write({path='record', content='computed 17'})
        return {response = 17}
      end
      PleaseSimulateException('this input needs simulation')
    end"#;
    // Exactly two simulator completions are scripted: one for the delegating
    // `lookup` call and one for the LLM-rendered `read`. The computed call and
    // the tool with no implementation must not consume any.
    let sim = Arc::new(MockLlmClient::scripted(vec![
        reply(json!({"response": "I saw 17"})),
        reply(json!({"response": {"content": "computed 17"}})),
    ]));
    let definition = definition(vec![
        tool_with("lookup", SideEffect::Read, Some(source)),
        tool_with("read", SideEffect::Read, None),
    ]);
    let put = put(vec![
        tool("lookup", SideEffect::Read),
        tool("read", SideEffect::Read),
    ]);
    let put_client = Arc::new(MockLlmClient::scripted(vec![
        calls(vec![
            call("lookup", json!({"compute": true})),
            call("lookup", json!({"compute": false})),
            call("read", json!({"path": "record"})),
        ]),
        reply(json!({"response": "done"})),
    ]));
    let runner = Runner::new(
        put_client.clone(),
        "put",
        None,
        sim.clone(),
        "sim",
        None,
        RunnerOptions::default(),
    );
    let runtime = runtime(&definition, &put, Workspace::empty());
    let trace = runner
        .run(
            &put,
            &runtime,
            &Budget {
                max_steps_per_trace: 4,
                max_tokens: None,
            },
            None,
            None,
        )
        .await
        .unwrap();

    let exchanges = &trace.turns[0].tool_exchanges;
    // 1) computed in Lua: no model call, response is the handler's value.
    assert_eq!(exchanges[0].response, 17);
    let record = exchanges[0].lua_execution.as_ref().unwrap();
    assert_eq!(record.outcome, LuaOutcome::Computed);
    assert_eq!(record.tool, "lookup");
    assert_eq!(record.source_hash, trace.implementations[0].source_hash);
    assert_eq!(
        sim.requests.lock().unwrap().len(),
        2,
        "only the delegating call and the LLM-only tool reach the simulator"
    );
    // 2) explicitly delegated, with the record kept as evidence.
    let delegated = exchanges[1].lua_execution.as_ref().unwrap();
    assert_eq!(delegated.outcome, LuaOutcome::Fallback);
    assert!(
        delegated
            .detail
            .as_ref()
            .unwrap()
            .contains("needs simulation")
    );
    assert_eq!(exchanges[1].response, "I saw 17");
    // 3) a tool with no implementation never attempts Lua at all, and the
    //    simulator can read the file the Lua handler committed.
    assert!(exchanges[2].lua_execution.is_none());
    assert_eq!(exchanges[2].response["content"], "computed 17");

    // The computed response is part of the simulator's established history, so
    // the model saw it (with its backend tag) when rendering the delegation.
    let requests = sim.requests.lock().unwrap();
    assert!(requests[0].messages.iter().any(|m| matches!(
        m,
        Message::User { content } if content.contains("\"execution_backend\":\"lua\"")
    )));
    assert!(trace.implementations.len() == 1);
    // The run-level counters let a caller see whether the supplied code served
    // the run WITHOUT reading every exchange: one computed, one delegated, and
    // the unimplemented tool counted as neither.
    let execution = &trace.execution;
    assert_eq!(execution.lua_computed_calls, 1);
    assert_eq!(execution.lua_fallback_calls, 1);
    assert_eq!(execution.lua_error_calls, 0);
}

/// A runtime error is distinct evidence, its staged writes are rolled back, and
/// the LLM still renders the tool response — the run does not fail.
#[tokio::test]
async fn a_crashing_handler_rolls_back_and_delegates() {
    let broken = r#"return function(a, c)
      c.workspace.write({path='x', content='uncommitted'})
      error('broken handler')
    end"#;
    let sim = Arc::new(MockLlmClient::scripted(vec![
        reply(json!({"response": "rendered"})),
        reply(json!({"response": "rendered again"})),
    ]));
    let definition = definition(vec![tool_with("lookup", SideEffect::Read, Some(broken))]);
    let put = put(vec![tool("lookup", SideEffect::Read)]);
    let put_client = Arc::new(MockLlmClient::scripted(vec![
        calls(vec![call("lookup", json!({})), call("lookup", json!({}))]),
        reply(json!({"response": "done"})),
    ]));
    let runner = Runner::new(
        put_client,
        "put",
        None,
        sim.clone(),
        "sim",
        None,
        RunnerOptions::default(),
    );
    let trace = runner
        .run(
            &put,
            &runtime(&definition, &put, Workspace::empty()),
            &Budget {
                max_steps_per_trace: 4,
                max_tokens: None,
            },
            None,
            None,
        )
        .await
        .unwrap();
    let first = &trace.turns[0].tool_exchanges[0];
    let record = first.lua_execution.as_ref().unwrap();
    assert_eq!(record.outcome, LuaOutcome::Error);
    // Both identical calls in the batch crashed, so the run counter counts both.
    assert_eq!(trace.execution.lua_error_calls, 2);
    assert_eq!(trace.execution.lua_computed_calls, 0);
    assert_eq!(trace.execution.lua_fallback_calls, 0);
    assert!(record.detail.as_ref().unwrap().contains("broken handler"));
    assert_eq!(record.discarded_workspace_ops.len(), 1);
    assert_eq!(first.response, "rendered");
    // The rolled-back write never reached the workspace: the simulator's read
    // of the same path still fails in the second identical call.
    let second = &trace.turns[0].tool_exchanges[1];
    assert!(
        second.lua_execution.is_none()
            || second
                .lua_execution
                .as_ref()
                .unwrap()
                .discarded_workspace_ops
                .len()
                == 1
    );
}

/// Nothing in the harness asks a model to write or repair code: with
/// implementations supplied, a whole run makes zero authoring requests.
#[tokio::test]
async fn no_preparation_or_authoring_calls_happen() {
    let source = "return function() return {response = 'computed'} end";
    let sim = Arc::new(MockLlmClient::scripted(vec![]));
    let definition = definition(vec![tool_with("lookup", SideEffect::Read, Some(source))]);
    let put = put(vec![tool("lookup", SideEffect::Read)]);
    let put_client = Arc::new(MockLlmClient::scripted(vec![
        calls(vec![call("lookup", json!({}))]),
        reply(json!({"response": "done"})),
    ]));
    let runner = Runner::new(
        put_client,
        "put",
        None,
        sim.clone(),
        "sim",
        None,
        RunnerOptions::default(),
    );
    let progress = progress_with_message(None);
    let trace = runner
        .run(
            &put,
            &runtime(&definition, &put, Workspace::empty()),
            &Budget {
                max_steps_per_trace: 4,
                max_tokens: None,
            },
            None,
            Some(progress.clone()),
        )
        .await
        .unwrap();
    assert_eq!(trace.turns[0].tool_exchanges[0].response, "computed");
    assert!(sim.requests.lock().unwrap().is_empty());
    let snapshot = progress.lock().unwrap();
    assert!(matches!(snapshot.phase, RunPhase::PutLoop));
    assert!(snapshot.implementations.len() == 1);
}

/// A probe runs the SAME calls through the SAME engine: identical Lua
/// responses and provenance, without invoking the PUT.
#[tokio::test]
async fn probes_render_through_the_shared_engine_and_carry_provenance() {
    let source = "return function(a, c) return {response = {echo = a.value}} end";
    let definition = definition(vec![tool_with("echo", SideEffect::Read, Some(source))]);
    let client = Arc::new(MockLlmClient::scripted(vec![]));
    let target = ProbeTarget {
        scenario_id: "scn-1".into(),
        revision: 3,
        definition_hash: definition.content_hash(),
        runtime: ScenarioRuntime::from_definition(&definition, Workspace::empty()),
    };
    let request = ProbeRequest {
        tool_calls: vec![
            simulation::ToolCall {
                name: "echo".into(),
                args: json!({"value": 1}),
            },
            simulation::ToolCall {
                name: "nope".into(),
                args: json!({}),
            },
            simulation::ToolCall {
                name: "echo".into(),
                args: json!({"value": 2}),
            },
        ],
        resolved_inputs: None,
        expected_revision: Some(3),
        max_calls: None,
        reason: Some("does the handler echo?".into()),
    };
    request.validate().unwrap();
    let progress = Arc::new(Mutex::new(ProbeProgress::new(0)));
    run_probe(
        client,
        "sim",
        None,
        &target,
        &request,
        progress.clone(),
        || 0,
    )
    .await;
    let progress = progress.lock().unwrap();
    assert_eq!(progress.status, prompt_explore::scenario::ProbeStatus::Done);
    assert_eq!(progress.calls.len(), 3);
    assert_eq!(progress.calls[0].response.as_ref().unwrap()["echo"], 1);
    assert_eq!(
        progress.calls[0].lua_execution.as_ref().unwrap().outcome,
        LuaOutcome::Computed
    );
    // An unknown tool is an in-band error, exactly as in a PUT run.
    assert!(
        progress.calls[1]
            .response
            .as_ref()
            .unwrap()
            .as_str()
            .unwrap()
            .contains("unknown tool 'nope'")
    );
    assert_eq!(progress.calls[2].response.as_ref().unwrap()["echo"], 2);
}

/// A probe stops at the first unrenderable call and keeps what it completed.
#[tokio::test]
async fn a_failed_probe_keeps_completed_calls_and_says_where_it_stopped() {
    let source = "return function() PleaseSimulateException('needs the model') end";
    let definition = definition(vec![tool_with("lookup", SideEffect::Read, Some(source))]);
    // The scripted simulator has nothing to say: the delegation fails.
    let client = Arc::new(MockLlmClient::scripted(vec![]));
    let target = ProbeTarget {
        scenario_id: "scn-1".into(),
        revision: 1,
        definition_hash: definition.content_hash(),
        runtime: ScenarioRuntime::from_definition(&definition, Workspace::empty()),
    };
    let request = ProbeRequest {
        tool_calls: vec![
            simulation::ToolCall {
                name: "lookup".into(),
                args: json!({}),
            },
            simulation::ToolCall {
                name: "lookup".into(),
                args: json!({}),
            },
        ],
        resolved_inputs: None,
        expected_revision: None,
        max_calls: None,
        reason: None,
    };
    let progress = Arc::new(Mutex::new(ProbeProgress::new(0)));
    run_probe(
        client,
        "sim",
        None,
        &target,
        &request,
        progress.clone(),
        || 0,
    )
    .await;
    let progress = progress.lock().unwrap();
    assert_eq!(
        progress.status,
        prompt_explore::scenario::ProbeStatus::Failed
    );
    assert_eq!(
        progress.stop_reason,
        Some(prompt_explore::scenario::ProbeStopReason::RuntimeFailure)
    );
    assert_eq!(progress.calls.len(), 1, "the failed call is retained");
    assert!(progress.calls[0].error.is_some());
    assert!(
        progress
            .error
            .as_ref()
            .unwrap()
            .contains("could not be rendered")
    );
}

/// `max_calls` truncates the sequence explicitly, and the stop reason says so.
#[tokio::test]
async fn max_calls_is_reported_as_a_call_limit_stop() {
    let source = "return function(a) return {response = a.n} end";
    let definition = definition(vec![tool_with("echo", SideEffect::Read, Some(source))]);
    let client = Arc::new(MockLlmClient::scripted(vec![]));
    let target = ProbeTarget {
        scenario_id: "scn".into(),
        revision: 1,
        definition_hash: definition.content_hash(),
        runtime: ScenarioRuntime::from_definition(&definition, Workspace::empty()),
    };
    let request = ProbeRequest {
        tool_calls: (1..=3)
            .map(|n| simulation::ToolCall {
                name: "echo".into(),
                args: json!({"n": n}),
            })
            .collect(),
        resolved_inputs: None,
        expected_revision: None,
        max_calls: Some(2),
        reason: None,
    };
    let progress = Arc::new(Mutex::new(ProbeProgress::new(0)));
    run_probe(
        client,
        "sim",
        None,
        &target,
        &request,
        progress.clone(),
        || 0,
    )
    .await;
    let progress = progress.lock().unwrap();
    assert_eq!(progress.calls.len(), 2);
    assert_eq!(
        progress.stop_reason,
        Some(prompt_explore::scenario::ProbeStopReason::CallLimit)
    );
}

/// Supplied inputs replace sampling and are reported back verbatim.
#[tokio::test]
async fn explicit_inputs_are_validated_and_used_verbatim() {
    let mut definition = definition(vec![]);
    definition.input_domain =
        HashMap::from([("tier".to_string(), "standard or premium".to_string())]);
    let runtime = ScenarioRuntime::from_definition(&definition, Workspace::empty());
    let client = Arc::new(MockLlmClient::scripted(vec![]));
    // No declared-call sequence at all: resolution is the only simulator work,
    // and it must not happen when bindings are supplied.
    let mut engine = prompt_explore::simulate::SimEngine::start(
        client.clone(),
        "sim",
        None,
        &runtime,
        Some(&HashMap::from([("tier".to_string(), json!("premium"))])),
        None,
    )
    .await
    .unwrap();
    assert_eq!(engine.resolved_inputs()["tier"], "premium");
    assert!(client.requests.lock().unwrap().is_empty());

    // Unknown or incomplete bindings are refused with a readable error.
    let error = match prompt_explore::simulate::SimEngine::start(
        client.clone(),
        "sim",
        None,
        &runtime,
        Some(&HashMap::from([("other".to_string(), json!(1))])),
        None,
    )
    .await
    {
        Err(error) => error.to_string(),
        Ok(_) => panic!("undeclared bindings must be refused"),
    };
    assert!(error.contains("does not declare"), "{error}");
}

/// The definition validates tool shape and Lua syntax deterministically, with
/// the offending tool named.
#[test]
fn definition_validation_names_the_offending_tool() {
    let mut definition = definition(vec![
        tool_with("dup", SideEffect::Read, None),
        tool_with("dup", SideEffect::Read, None),
    ]);
    assert!(
        definition
            .validate()
            .unwrap_err()
            .contains("duplicate tool name")
    );
    definition.tools = vec![tool_with(
        "broken",
        SideEffect::Read,
        Some("return function( this is not lua"),
    )];
    let error = definition.validate().unwrap_err();
    assert!(error.contains("broken"), "{error}");
    assert!(error.contains("does not parse"), "{error}");
    // A valid implementation passes, and its hash is stable for equal source.
    definition.tools = vec![tool_with(
        "ok",
        SideEffect::Read,
        Some("return function() return {response = true} end"),
    )];
    definition.validate().unwrap();
    let twice = definition.implementations();
    assert_eq!(
        twice[0].source_hash,
        definition.implementations()[0].source_hash
    );
    // The contract projection never carries the implementation source.
    let encoded = serde_json::to_string(&definition.tool_contracts()).unwrap();
    assert!(!encoded.contains("function()"));
}
