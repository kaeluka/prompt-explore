//! The evaluation loop: run one explicit scenario against a PUT and surface
//! complete evidence. There is NO judge here — the caller reads the trace and
//! decides. Scenarios are authored outside the harness.

use std::sync::Arc;

use crate::llm::LlmClient;
use crate::model::input::{Investigation, PromptUnderTest};
use crate::model::output::RunFailure;
use crate::model::simulation::{RunProgress, RunStopReason, Scenario, Trace};
use crate::simulate::{Runner, RunnerOptions, Workspace};

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
    /// The simulation-workspace seed (an uploaded zip, or empty). Every
    /// Runner session clones it, keeping direct and investigator callers'
    /// trace workspaces isolated while cheaply sharing the immutable seed.
    pub workspace_seed: Workspace,
    /// Controls for every LLM conversation in a trace.
    pub runner_options: RunnerOptions,
}

/// Evidence from one scenario conversation. Exactly one of `trace` and
/// `failure` is present. On failure, `progress` (when supplied) retains any
/// turns, simulation program revisions, and resolved inputs produced before
/// the error.
pub struct InvestigateOutcome {
    /// The scenario that was run, echoed by value for inspection.
    pub scenario: Scenario,
    /// The completed trace, when the conversation succeeded.
    pub trace: Option<Trace>,
    /// The captured error, when the conversation failed.
    pub failure: Option<RunFailure>,
}

impl Investigator {
    /// Run exactly one scenario against the PUT. The investigation's `reason`
    /// is advisory framing for the caller; nothing here is judged against it.
    /// If supplied, progress is initialized for this scenario and updated live.
    pub async fn investigate(
        &self,
        investigation: &Investigation,
        put: &PromptUnderTest,
        scenario: &Scenario,
        progress: Option<Arc<std::sync::Mutex<RunProgress>>>,
    ) -> InvestigateOutcome {
        // Keep progress even for standalone callers: the runner returns its
        // execution evidence in the successful Trace, while this handle lets
        // failure and panic paths retain the same evidence until finalization.
        let progress =
            progress.unwrap_or_else(|| Arc::new(std::sync::Mutex::new(RunProgress::default())));
        if let Ok(mut current) = progress.lock() {
            current.initialize(scenario.user_message.clone());
        }

        // Keep this one conversation in its own task so a panic in a client or
        // runner is returned as ordinary failure evidence rather than escaping
        // the server's job task and leaving it permanently running. This is not
        // batch orchestration: exactly one task and one scenario are awaited.
        let put_role = self.runner_put.clone();
        let sim_role = self.runner_sim.clone();
        let workspace_seed = self.workspace_seed.clone();
        let runner_options = self.runner_options.clone();
        let put = put.clone();
        let task_scenario = scenario.clone();
        let budget = investigation.budget.clone();
        let task_progress = progress.clone();
        let task = tokio::spawn(async move {
            let runner = Runner::new(
                put_role.client,
                put_role.model,
                put_role.thinking_level,
                sim_role.client,
                sim_role.model,
                sim_role.thinking_level,
                workspace_seed,
                runner_options,
            );
            runner
                .run(&put, &task_scenario, &budget, Some(task_progress))
                .await
        });

        match task.await {
            Ok(Ok(trace)) => InvestigateOutcome {
                scenario: scenario.clone(),
                trace: Some(trace),
                failure: None,
            },
            Ok(Err(error)) => {
                finish_failure(&progress);
                InvestigateOutcome {
                    scenario: scenario.clone(),
                    trace: None,
                    failure: Some(RunFailure {
                        stage: "runner".into(),
                        error: error.to_string(),
                    }),
                }
            }
            Err(join_error) => {
                finish_failure(&progress);
                InvestigateOutcome {
                    scenario: scenario.clone(),
                    trace: None,
                    failure: Some(RunFailure {
                        stage: "runner".into(),
                        error: format!("task panicked: {join_error}"),
                    }),
                }
            }
        }
    }
}

fn finish_failure(progress: &Arc<std::sync::Mutex<RunProgress>>) {
    if let Ok(mut progress) = progress.lock() {
        progress.finish(RunStopReason::RuntimeFailure);
    }
}
