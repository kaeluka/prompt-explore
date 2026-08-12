//! The evaluation loop: run explicit scenarios against a PUT and
//! surface complete evidence (traces). There is NO judge here — the
//! caller reads the traces and decides.
//!
//! There is no scenario generation here. Scenarios are authored outside
//! the harness (by the operator's agent) and passed in; an explicit list
//! is a contract — all of them are run. Run errors are captured, not
//! swallowed: a scenario whose trace fails becomes a `ScenarioFailure`,
//! so an empty `attempts` list is interpretable.

use std::sync::Arc;

use tokio::task::JoinSet;

use crate::llm::LlmClient;
use crate::model::input::{Investigation, PromptUnderTest};
use crate::model::output::{RunResult, RunStatus, ScenarioFailure};
use crate::model::simulation::Scenario;
use crate::simulate::{Runner, Workspace};


/// One LLM client + model name, reused across runner roles.
#[derive(Clone)]
pub struct LlmRole {
    pub client: Arc<dyn LlmClient>,
    pub model: String,
}

pub struct Investigator {
    pub runner_put: LlmRole,
    pub runner_sim: LlmRole,
    /// The simulation-workspace seed (an uploaded zip, or empty). Cloned
    /// per trace so every scenario run gets an isolated workspace; the
    /// seed itself is shared by `Arc` so a large upload is paid for once.
    pub workspace_seed: Workspace,
}

pub struct InvestigateOutcome {
    pub result: RunResult,
    /// The scenarios that were run (echoed back for inspection).
    pub scenarios: Vec<Scenario>,
    /// Every completed run: scenario + trace. Essential for auditing
    /// negative results ("did it even try?") and for the caller to
    /// judge — every trace is the evidence.
    pub attempts: Vec<Attempt>,
}

pub struct Attempt {
    pub scenario: Scenario,
    pub trace: crate::model::simulation::Trace,
}

/// Outcome of running one scenario: a trace, or a captured failure.
enum RunOne {
    Done(usize, Scenario, crate::model::simulation::Trace),
    Failed(usize, Scenario, &'static str, String),
}

impl Investigator {
    /// Run exactly the given scenarios against the PUT. All of them —
    /// an explicit list is a contract. If `progress` is given, it's
    /// populated live (steps as simulated, states as tasks finish) for
    /// polling/UI. The investigation's `question` is advisory framing
    /// for the caller; nothing here is judged against it.
    pub async fn investigate(
        &self,
        investigation: &Investigation,
        put: &PromptUnderTest,
        scenarios: &[Scenario],
        progress: Option<Arc<std::sync::Mutex<crate::model::simulation::RunProgress>>>,
    ) -> InvestigateOutcome {
        if let Some(p) = &progress {
            if let Ok(mut g) = p.lock() {
                g.scenarios = scenarios
                    .iter()
                    .map(|s| crate::model::simulation::ScenarioProgress {
                        state: crate::model::simulation::ScenarioState::Running,
                        steps: Vec::new(),
                        user_message: s.user_message.clone(),
                        resolved_inputs: Default::default(),
                    })
                    .collect();
            }
        }

        let mut tasks: JoinSet<RunOne> = JoinSet::new();
        for (index, scenario) in scenarios.iter().enumerate() {
            self.spawn_run(
                &mut tasks,
                &investigation.budget,
                put,
                scenario.clone(),
                index,
                progress.clone(),
            );
        }

        let mut attempts = Vec::new();
        let mut failures = Vec::new();
        while let Some(res) = tasks.join_next().await {
            match res {
                Ok(RunOne::Done(index, scenario, trace)) => {
                    if let Some(p) = &progress {
                        if let Ok(mut g) = p.lock() {
                            g.set_state(
                                index,
                                crate::model::simulation::ScenarioState::Done,
                            );
                        }
                    }
                    attempts.push(Attempt { scenario, trace });
                }
                Ok(RunOne::Failed(index, scenario, stage, error)) => {
                    if let Some(p) = &progress {
                        if let Ok(mut g) = p.lock() {
                            g.set_state(
                                index,
                                crate::model::simulation::ScenarioState::Failed {
                                    stage: stage.into(),
                                    error: error.clone(),
                                },
                            );
                        }
                    }
                    failures.push(ScenarioFailure {
                        scenario,
                        stage: stage.into(),
                        error,
                    })
                }
                Err(join_err) => failures.push(ScenarioFailure {
                    scenario: Scenario {
                        world: "<runner task panicked before returning its scenario>".into(),
                        input_domain: Default::default(),
                        user_message: None,
                        simulator_notes: String::new(),
                    },
                    stage: "runner".into(),
                    error: format!("task panicked: {join_err}"),
                }),
            }
        }

        // Run-completion taxonomy (no verdict): purely about whether
        // scenarios produced traces. All failed -> error; some failed
        // -> partial; otherwise completed. The caller judges the traces.
        let status = if attempts.is_empty() && !failures.is_empty() {
            RunStatus::Error
        } else if !failures.is_empty() {
            RunStatus::Partial
        } else {
            RunStatus::Completed
        };
        let final_state = attempts.last().map(|a| a.trace.final_world_state.clone());

        InvestigateOutcome {
            result: RunResult {
                status,
                scenarios_tried: scenarios.len() as u32,
                failures,
                final_state,
            },
            scenarios: scenarios.to_vec(),
            attempts,
        }
    }

    /// Spawn one run task for a scenario. Errors are captured as
    /// `RunOne::Failed` rather than swallowed.
    fn spawn_run(
        &self,
        tasks: &mut JoinSet<RunOne>,
        budget: &crate::model::Budget,
        put: &PromptUnderTest,
        scenario: Scenario,
        index: usize,
        progress: Option<Arc<std::sync::Mutex<crate::model::simulation::RunProgress>>>,
    ) {
        let put_role = self.runner_put.clone();
        let sim_role = self.runner_sim.clone();
        let budget = budget.clone();
        let workspace_seed = self.workspace_seed.clone();
        let put_template = put.template.clone();
        let put_tools = put.tools.clone();
        tasks.spawn(async move {
            let runner = Runner::new(
                put_role.client,
                &put_role.model,
                sim_role.client,
                &sim_role.model,
                workspace_seed,
            );

            // A lightweight PUT view for the runner (design_goals are
            // caller documentation, not used by the runner).
            let put_view = crate::model::input::PromptUnderTest {
                id: String::new(),
                template: put_template,
                tools: put_tools,
                design_goals: String::new(),
            };
            let trace = match runner
                .run(&put_view, &scenario, &budget, index, progress.clone())
                .await
            {
                Ok(t) => t,
                Err(e) => return RunOne::Failed(index, scenario, "runner", e.to_string()),
            };
            RunOne::Done(index, scenario, trace)
        });
    }
}
