//! Hybrid orchestration, with scripted LLMs: executable and rendered replies
//! share one conversation, one workspace and one world-state trajectory.
use prompt_explore::llm::{ChatResponse, Message, MockLlmClient, ToolCallRequest};
use prompt_explore::model::simulation::{LuaOutcome, RunPhase, RunProgress};
use prompt_explore::model::*;
use prompt_explore::simulate::lua::{LuaOptions, PROGRAM_PATH};
use prompt_explore::simulate::{Runner, RunnerOptions, SimulatorOptions, ToolSimulator, Workspace};
use serde_json::json;
use std::sync::{Arc, Mutex};

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
fn write_program(source: &str) -> ChatResponse {
    calls(vec![call(
        "write",
        json!({"path":PROGRAM_PATH,"content":source}),
    )])
}
fn tool(name: &str, side_effect: SideEffect) -> ToolSchema {
    ToolSchema {
        name: name.into(),
        description: name.into(),
        parameters: json!({"type":"object"}),
        side_effect,
        example_responses: vec![],
    }
}
fn options() -> SimulatorOptions {
    SimulatorOptions {
        lua_simulation: Some(LuaOptions::default()),
        ..Default::default()
    }
}
fn simulator(client: Arc<MockLlmClient>) -> ToolSimulator {
    ToolSimulator::new(client, "sim", None, Workspace::empty(), options())
}

#[tokio::test]
async fn initialized_stubs_delegate_and_do_not_require_specialization() {
    let client = Arc::new(MockLlmClient::scripted(vec![
        reply(json!({"ready":true})),
        reply(json!({"response":"rendered"})),
    ]));
    let mut session = simulator(client.clone()).session("one tool");
    let t = tool("lookup", SideEffect::Read);
    session.prepare_program(&[t.clone()]).await.unwrap();
    let outcome = session
        .respond(
            &t,
            &simulation::ToolCall {
                name: t.name.clone(),
                args: json!({}),
            },
            &Default::default(),
        )
        .await
        .unwrap();
    assert_eq!(outcome.response, "rendered");
    assert_eq!(outcome.lua_execution.unwrap().outcome, LuaOutcome::Fallback);
    assert_eq!(client.requests.lock().unwrap().len(), 2);
    let program = session.simulation_program().unwrap();
    assert_eq!(program.revisions.len(), 1);
    assert!(
        program.revisions[0]
            .source
            .contains("PleaseSimulateException")
    );
}

#[tokio::test]
async fn computed_and_rendered_completions_share_history_and_workspace() {
    let source = r#"return { lookup=function(a,c)
      if a.compute then c.workspace.write({path='record',content='computed 17'}); return {response=17} end
      PleaseSimulateException('this input needs simulation')
    end }"#;
    let client = Arc::new(MockLlmClient::scripted(vec![
        write_program(source),
        reply(json!({"ready":true})),
        calls(vec![call("read", json!({"path":"record"}))]),
        reply(json!({"response":"I saw 17"})),
    ]));
    let mut session = simulator(client.clone())
        .session("lookup compute returns 17; other inputs describe prior actions");
    let t = tool("lookup", SideEffect::Read);
    session.prepare_program(&[t.clone()]).await.unwrap();
    assert_eq!(client.requests.lock().unwrap().len(), 2);
    let computed = session
        .respond(
            &t,
            &simulation::ToolCall {
                name: t.name.clone(),
                args: json!({"compute":true}),
            },
            &Default::default(),
        )
        .await
        .unwrap();
    assert_eq!(computed.response, 17);
    assert_eq!(
        computed.lua_execution.unwrap().outcome,
        LuaOutcome::Computed
    );
    assert_eq!(
        client.requests.lock().unwrap().len(),
        2,
        "computed response must not call the model"
    );
    let rendered = session
        .respond(
            &t,
            &simulation::ToolCall {
                name: t.name.clone(),
                args: json!({"compute":false}),
            },
            &Default::default(),
        )
        .await
        .unwrap();
    assert_eq!(
        rendered.lua_execution.unwrap().outcome,
        LuaOutcome::Fallback
    );
    assert_eq!(rendered.workspace_ops[0].result["content"], "computed 17");
    let requests = client.requests.lock().unwrap();
    assert!(requests[2].messages.iter().any(
        |m| matches!(m,Message::Assistant {content:Some(s),..} if s.contains("\"response\":17"))
    ));
    assert!(requests[2].messages.iter().any(
        |m| matches!(m,Message::User {content} if content.contains("\"execution_backend\":\"lua\""))
    ));
}

#[tokio::test]
async fn crash_rolls_back_before_llm_fallback_and_later_specialization_is_versioned() {
    let broken = "return { lookup=function(a,c) c.workspace.write({path='x',content='uncommitted'}); error('broken handler') end }";
    let repaired = "return { lookup=function() return {response='computed after repair'} end }";
    let client = Arc::new(MockLlmClient::scripted(vec![
        write_program(broken),
        reply(json!({"ready":true})),
        calls(vec![
            call("read", json!({"path":"x"})),
            call("write", json!({"path":PROGRAM_PATH,"content":repaired})),
        ]),
        reply(json!({"response":"rendered"})),
    ]));
    let mut session =
        simulator(client.clone()).session("x must not persist after a failed computed call");
    let t = tool("lookup", SideEffect::Read);
    session.prepare_program(&[t.clone()]).await.unwrap();
    let c = simulation::ToolCall {
        name: t.name.clone(),
        args: json!({}),
    };
    let outcome = session.respond(&t, &c, &Default::default()).await.unwrap();
    let record = outcome.lua_execution.unwrap();
    assert_eq!(record.outcome, LuaOutcome::Error);
    assert_eq!(record.program_revision, 1);
    assert!(record.detail.unwrap().contains("broken handler"));
    assert_eq!(record.discarded_workspace_ops.len(), 1);
    assert!(outcome.workspace_ops[0].result.get("error").is_some());
    let next = session.respond(&t, &c, &Default::default()).await.unwrap();
    assert_eq!(next.response, "computed after repair");
    assert_eq!(next.lua_execution.unwrap().program_revision, 2);
    assert_eq!(client.requests.lock().unwrap().len(), 4);
    assert_eq!(
        session.simulation_program().unwrap().revisions[1].source,
        broken
    );
    assert_eq!(
        session.simulation_program().unwrap().revisions[2].source,
        repaired
    );
}

#[tokio::test]
async fn atomic_batch_preserves_write_read_fallback_state_and_publishes_program() {
    let source = r#"return {
      set=function(a,c) return {response={stock=a.stock},state_patch={stock=a.stock}} end,
      get=function(a,c) return {response={stock=c.world_state.stock}} end,
      audit=function() PleaseSimulateException('read the preceding computed actions') end
    }"#;
    let sim = Arc::new(MockLlmClient::scripted(vec![
        write_program(source),
        reply(json!({"ready":true})),
        reply(json!({"response":{"stock":11,"summary":"set then read"}})),
    ]));
    let put_client = Arc::new(MockLlmClient::scripted(vec![calls(vec![
        call("set", json!({"stock":11})),
        call("get", json!({})),
        call("audit", json!({})),
    ])]));
    let put = PromptUnderTest {
        id: "state".into(),
        template: "Set, read, audit".into(),
        tools: vec![
            tool("set", SideEffect::Write),
            tool("get", SideEffect::Read),
            tool("audit", SideEffect::Read),
        ],
        design_goals: String::new(),
    };
    let scenario = Scenario { world:"Stock initially zero; set changes it, get returns it, audit reports the actual actions.".into(),input_domain:Default::default(),user_message:Some("Set stock to 11, read, audit".into()),simulator_notes:String::new() };
    let mut initial_progress = RunProgress::default();
    initial_progress.user_message = scenario.user_message.clone();
    let progress = Arc::new(Mutex::new(initial_progress));
    let runner = Runner::new(
        put_client.clone(),
        "put",
        None,
        sim.clone(),
        "sim",
        None,
        Workspace::empty(),
        RunnerOptions {
            simulator: options(),
            ..Default::default()
        },
    );
    let trace = runner
        .run(
            &put,
            &scenario,
            &Budget {
                max_steps_per_trace: 2,
                max_tokens: None,
            },
            Some(progress.clone()),
        )
        .await
        .unwrap();
    assert_eq!(trace.turns.len(), 1);
    assert_eq!(
        trace.turns[0].tool_exchanges.len(),
        3,
        "accepted batches still finish across the cap"
    );
    assert_eq!(trace.final_world_state["stock"], 11);
    assert_eq!(trace.turns[0].tool_exchanges[1].response["stock"], 11);
    assert_eq!(put_client.requests.lock().unwrap().len(), 1);
    assert_eq!(
        sim.requests.lock().unwrap().len(),
        3,
        "only setup and audit use the LLM"
    );
    let snapshot = progress.lock().unwrap();
    assert!(matches!(snapshot.phase, RunPhase::PutLoop));
    assert_eq!(
        snapshot
            .simulation_program
            .as_ref()
            .unwrap()
            .revisions
            .len(),
        2
    );
    let encoded = serde_json::to_value(trace).unwrap();
    assert_eq!(
        encoded["simulation_program"]["revisions"][1]["source"],
        source
    );
    assert_eq!(
        encoded["turns"][0]["tool_exchanges"][2]["lua_execution"]["outcome"],
        "fallback"
    );
}

#[tokio::test]
async fn preparation_failure_keeps_program_and_resolved_inputs_visible_before_any_put_turn() {
    let sim = Arc::new(MockLlmClient::scripted(vec![reply(json!({"n":7}))]));
    let put_client = Arc::new(MockLlmClient::scripted(vec![]));
    let put = PromptUnderTest {
        id: "setup-failure".into(),
        template: "Read {{n}}".into(),
        tools: vec![tool("lookup", SideEffect::Read)],
        design_goals: String::new(),
    };
    let scenario = Scenario {
        world: "Only item 7 exists.".into(),
        input_domain: std::collections::HashMap::from([(
            "n".into(),
            "The sole item number".into(),
        )]),
        user_message: None,
        simulator_notes: String::new(),
    };
    let progress = Arc::new(Mutex::new(RunProgress::default()));
    let runner = Runner::new(
        put_client.clone(),
        "put",
        None,
        sim,
        "sim",
        None,
        Workspace::empty(),
        RunnerOptions {
            simulator: options(),
            ..Default::default()
        },
    );
    assert!(
        runner
            .run(
                &put,
                &scenario,
                &Budget {
                    max_steps_per_trace: 4,
                    max_tokens: None
                },
                Some(progress.clone())
            )
            .await
            .is_err()
    );
    let snapshot = progress.lock().unwrap();
    assert!(matches!(snapshot.phase, RunPhase::PreparingTools));
    assert_eq!(snapshot.resolved_inputs["n"], 7);
    assert!(snapshot.turns.is_empty());
    assert!(
        snapshot.simulation_program.as_ref().unwrap().revisions[0]
            .source
            .contains("PleaseSimulateException")
    );
    assert!(put_client.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn enabled_mode_skips_preparation_for_no_tool_puts() {
    let sim = Arc::new(MockLlmClient::scripted(vec![]));
    let mut session = simulator(sim.clone()).session("empty");
    session.prepare_program(&[]).await.unwrap();
    assert!(session.simulation_program().is_none());
    assert!(sim.requests.lock().unwrap().is_empty());
}
