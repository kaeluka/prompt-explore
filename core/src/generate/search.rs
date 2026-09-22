//! The evaluation loop: run one explicit scenario against a PUT and surface
//! complete evidence. There is NO judge here — the caller reads the trace and
//! decides. Scenarios are authored outside the harness.

use std::sync::Arc;

use crate::llm::LlmClient;
use crate::model::input::{Investigation, PromptUnderTest};
use crate::model::output::RunFailure;
use crate::model::simulation::{RunProgress, RunStopReason, Trace};
use crate::model::workflow::WorkflowProgram;
use crate::simulate::{RunnerOptions, ScenarioRuntime, run_workflow};

/// One LLM client + model name, reused across runner roles.
#[derive(Clone)]
pub struct LlmRole {
    pub client: Arc<dyn LlmClient>,
    pub model: String,
    /// Thinking level for this role's completions; `None` = the provider's
    /// default (no field sent). Per-role by design: setting the PUT's level
    /// never changes the simulator's, and vice versa.
    pub thinking_level: Option<crate::llm::types::ThinkingLevel>,
}

pub struct Investigator {
    pub runner_put: LlmRole,
    pub runner_sim: LlmRole,
    /// Controls for every LLM conversation in a trace. Simulator controls are
    /// resolved from the SCENARIO's settings by the caller.
    pub runner_options: RunnerOptions,
}

/// Evidence from one scenario conversation. Exactly one of `trace` and
/// `failure` is present. On failure, `progress` (when supplied) retains any
/// turns, simulation program revisions, and resolved inputs produced before
/// the error.
pub struct InvestigateOutcome {
    /// The completed trace, when the conversation succeeded.
    pub trace: Option<Trace>,
    /// The captured error, when the conversation failed.
    pub failure: Option<RunFailure>,
}

impl Investigator {
    /// Library shorthand for a one-agent run: builds the default workflow
    /// program with ordinary `params.prompt` / `params.model` / `params.controls`
    /// and runs it like any other program. The HTTP API has no such form; it
    /// always submits `workflow`.
    pub async fn investigate(
        &self,
        investigation: &Investigation,
        put: &PromptUnderTest,
        runtime: &ScenarioRuntime,
        resolved_inputs: Option<&std::collections::HashMap<String, serde_json::Value>>,
        progress: Option<Arc<std::sync::Mutex<RunProgress>>>,
    ) -> InvestigateOutcome {
        let workflow = WorkflowProgram {
            lua_source: crate::model::workflow::DEFAULT_WORKFLOW_LUA.into(),
            params: serde_json::json!({
                "prompt": put.template.clone(),
                "model": self.runner_put.model.clone(),
                "controls": {
                    "thinking": self.runner_put.thinking_level,
                    "temperature": self.runner_options.put_temperature,
                    "max_tokens": self.runner_options.put_max_tokens,
                }
            }),
            limits: Default::default(),
        };
        self.run_workflow_internal(investigation, runtime, resolved_inputs, progress, &workflow)
            .await
    }

    pub async fn investigate_workflow(
        &self,
        investigation: &Investigation,
        runtime: &ScenarioRuntime,
        resolved_inputs: Option<&std::collections::HashMap<String, serde_json::Value>>,
        progress: Option<Arc<std::sync::Mutex<RunProgress>>>,
        workflow: &WorkflowProgram,
    ) -> InvestigateOutcome {
        self.run_workflow_internal(investigation, runtime, resolved_inputs, progress, workflow)
            .await
    }

    async fn run_workflow_internal(
        &self,
        investigation: &Investigation,
        runtime: &ScenarioRuntime,
        resolved_inputs: Option<&std::collections::HashMap<String, serde_json::Value>>,
        progress: Option<Arc<std::sync::Mutex<RunProgress>>>,
        workflow: &WorkflowProgram,
    ) -> InvestigateOutcome {
        let progress =
            progress.unwrap_or_else(|| Arc::new(std::sync::Mutex::new(RunProgress::default())));
        if let Ok(mut current) = progress.lock() {
            current.initialize(runtime.scenario.user_message.clone());
        }

        let put_role = self.runner_put.clone();
        let sim_role = self.runner_sim.clone();
        let runner_options = self.runner_options.clone();
        let task_runtime = runtime.clone();
        let budget = investigation.budget.clone();
        let task_progress = progress.clone();
        let task_inputs = resolved_inputs.cloned();
        let workflow = workflow.clone();
        // Lua is local to this blocking thread, but async clients must keep
        // using the caller's long-lived runtime. A disposable per-workflow
        // runtime kills shared HTTP pool connection drivers when one run ends,
        // breaking concurrent investigations that reuse those connections.
        let handle = tokio::runtime::Handle::current();
        let task = tokio::task::spawn_blocking(move || {
            handle.block_on(run_workflow(
                put_role,
                sim_role,
                runner_options,
                budget,
                task_runtime,
                task_inputs,
                task_progress,
                workflow,
            ))
        });

        match task.await {
            Ok(Ok(trace)) => InvestigateOutcome {
                trace: Some(trace),
                failure: None,
            },
            Ok(Err(error)) => {
                finish_failure(&progress, &error.error);
                InvestigateOutcome {
                    trace: None,
                    failure: Some(RunFailure {
                        stage: error.stage.into(),
                        error: error.error,
                    }),
                }
            }
            Err(join_error) => {
                let error = format!("task panicked: {join_error}");
                finish_failure(&progress, &error);
                InvestigateOutcome {
                    trace: None,
                    failure: Some(RunFailure {
                        stage: "runner".into(),
                        error,
                    }),
                }
            }
        }
    }
}

fn finish_failure(progress: &Arc<std::sync::Mutex<RunProgress>>, error: &str) {
    if let Ok(mut progress) = progress.lock() {
        let reason = progress
            .workflow
            .as_ref()
            .and_then(|workflow| workflow.stop_reason)
            .filter(|reason| {
                matches!(
                    reason,
                    RunStopReason::StepBudget | RunStopReason::TokenBudget
                )
            })
            .unwrap_or(RunStopReason::RuntimeFailure);
        let turn_end = progress.turns.len();
        if let Some(workflow) = &mut progress.workflow {
            workflow.error = Some(error.into());
            workflow.stop_reason = Some(reason);
            for invocation in &mut workflow.invocations {
                if invocation.running {
                    invocation.running = false;
                    invocation.stop_reason = Some(RunStopReason::RuntimeFailure);
                    invocation.failure = Some(error.into());
                    invocation.turn_end = turn_end;
                }
            }
            for call in &mut workflow.tool_calls {
                if call.running {
                    call.running = false;
                    call.failure = Some(error.into());
                }
            }
        }
        progress.finish(reason);
    }
}
