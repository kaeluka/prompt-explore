//! The evaluation loop: run explicit scenarios against a PUT, judge each
//! trace, and on a witness propose (unverified) fixes.
//!
//! There is no scenario generation here. Scenarios are authored outside
//! the harness (by the operator's agent) and passed in; an explicit list
//! is a contract — all of them are run. Run errors are captured, not
//! swallowed: a scenario whose trace or verdict fails becomes a
//! `ScenarioFailure`, so an empty `attempts` list is interpretable.

use std::sync::Arc;

use tokio::task::JoinSet;

use crate::judge::Judge;
use crate::llm::LlmClient;
use crate::model::input::{Investigation, PromptUnderTest};
use crate::model::output::{Attribution, RunResult, RunStatus, ScenarioFailure, Witness};
use crate::model::predicate::{Predicate, SuccessMode};
use crate::model::simulation::Scenario;
use crate::simulate::Runner;


/// One LLM client + model name, reused across generator roles.
#[derive(Clone)]
pub struct LlmRole {
    pub client: Arc<dyn LlmClient>,
    pub model: String,
}

pub struct Investigator {
    pub runner_put: LlmRole,
    pub runner_sim: LlmRole,
    pub judge: LlmRole,
}

pub struct InvestigateOutcome {
    pub result: RunResult,
    /// The scenarios that were run (echoed back for inspection).
    pub scenarios: Vec<Scenario>,
    /// Every completed run: scenario + trace + verdict. Essential for
    /// auditing negative results ("did it even try?") and spotting
    /// near-misses.
    pub attempts: Vec<Attempt>,
}

pub struct Attempt {
    pub scenario: Scenario,
    pub trace: crate::model::simulation::Trace,
    /// Whether the judge matched this trace (denormalised from the
    /// trace's verdict for convenience).
    pub matched: bool,
}

/// Outcome of running one scenario: a judged trace, or a captured failure.
enum RunOne {
    Done(usize, Scenario, crate::model::simulation::Trace, bool),
    Failed(usize, Scenario, &'static str, String),
}

impl Investigator {
    /// Run exactly the given scenarios against the PUT. All of them —
    /// an explicit list is a contract. If `progress` is given, it's
    /// populated live (steps as simulated, states as tasks finish) for
    /// polling/UI.
    pub async fn investigate(
        &self,
        investigation: &Investigation,
        put: &PromptUnderTest,
        scenarios: &[Scenario],
        progress: Option<Arc<std::sync::Mutex<crate::model::simulation::RunProgress>>>,
    ) -> InvestigateOutcome {
        let predicate = Predicate {
            criterion: investigation.question.clone(),
            success_mode: SuccessMode::Witness,
        };

        if let Some(p) = &progress {
            if let Ok(mut g) = p.lock() {
                g.scenarios = scenarios
                    .iter()
                    .map(|s| crate::model::simulation::ScenarioProgress {
                        state: crate::model::simulation::ScenarioState::Running,
                        steps: Vec::new(),
                        user_message: s.user_message.clone(),
                    })
                    .collect();
            }
        }

        let mut tasks: JoinSet<RunOne> = JoinSet::new();
        for (index, scenario) in scenarios.iter().enumerate() {
            self.spawn_run(
                &mut tasks,
                &predicate,
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
                Ok(RunOne::Done(index, scenario, trace, matched)) => {
                    if let Some(p) = &progress {
                        if let Ok(mut g) = p.lock() {
                            g.set_state(
                                index,
                                crate::model::simulation::ScenarioState::Done { matched },
                            );
                        }
                    }
                    attempts.push(Attempt {
                        scenario,
                        trace: trace.clone(),
                        matched,
                    });
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

        let strategies_tried = (0..scenarios.len())
            .map(|i| format!("scenario #{i}"))
            .collect::<Vec<_>>();

        // All scenario tasks have finished; move into the design-goal
        // pass so a reader sees the job's current LLM phase, not a bare
        // "running" (this is the tail phase that can look like a stuck job).
        if let Some(p) = &progress {
            if let Ok(mut g) = p.lock() {
                g.set_phase(crate::model::simulation::RunPhase::CheckingGoals);
            }
        }

        // Advisory: surface design-goal violations across completed
        // traces as incidental findings. Best-effort (judge errors are
        // skipped, not fatal) and only when design_goals is non-empty.
        // These do NOT affect the witness verdict — the question is the
        // sole criterion; goals are surfaced for the operator to read.
        let mut incidental_findings: Vec<String> = Vec::new();
        let design_goals = put.design_goals.trim();
        if !design_goals.is_empty() {
            let goal_judge = Judge::new(self.judge.client.clone(), &self.judge.model);
            for (i, att) in attempts.iter().enumerate() {
                if let Ok(findings) = goal_judge
                    .check_goals(&att.trace, design_goals, Some(&att.scenario))
                    .await
                {
                    for f in findings.into_iter().filter(|f| f.violated) {
                        incidental_findings.push(format!(
                            "[attempt {i}] goal violated: {} — {}",
                            f.goal, f.rationale
                        ));
                    }
                }
            }
        }

        // Every scenario errored -> the run itself is an error, not a
        // clean "no witness".
        if attempts.is_empty() && !failures.is_empty() {
            return InvestigateOutcome {
                result: RunResult {
                    status: RunStatus::Error,
                    scenarios_tried: scenarios.len() as u32,
                    strategies_tried,
                    witness: None,
                    incidental_findings: incidental_findings.clone(),
                    failures,
                    final_state: None,
                },
                scenarios: scenarios.to_vec(),
                attempts,
            };
        }

        // Existential mode: the first matched attempt is the witness.
        if let Some(att) = attempts.iter().find(|a| a.matched) {
            let trace = &att.trace;
            let witness = Witness {
                attribution: Attribution {
                    instruction_spans: vec![],
                    evidence: "caller-provided witness scenario (see trace)".into(),
                },
                traces: vec![trace.clone()],
            };

            InvestigateOutcome {
                result: RunResult {
                    status: RunStatus::WitnessFound,
                    scenarios_tried: scenarios.len() as u32,
                    strategies_tried,
                    witness: Some(witness),
                    incidental_findings: incidental_findings.clone(),
                    failures,
                    final_state: Some(trace.final_world_state.clone()),
                },
                scenarios: scenarios.to_vec(),
                attempts,
            }
        } else {
            // No witness. Distinguish a complete sweep from a partial one.
            let status = if failures.is_empty() {
                RunStatus::NoWitnessWithinBudget
            } else {
                RunStatus::Partial
            };
            InvestigateOutcome {
                result: RunResult {
                    status,
                    scenarios_tried: scenarios.len() as u32,
                    strategies_tried,
                    witness: None,
                    incidental_findings: incidental_findings.clone(),
                    failures,
                    final_state: attempts.last().map(|a| a.trace.final_world_state.clone()),
                },
                scenarios: scenarios.to_vec(),
                attempts,
            }
        }
    }

    /// Spawn one run+judge task for a scenario. Errors are captured as
    /// `RunOne::Failed` rather than swallowed.
    fn spawn_run(
        &self,
        tasks: &mut JoinSet<RunOne>,
        predicate: &Predicate,
        budget: &crate::model::Budget,
        put: &PromptUnderTest,
        scenario: Scenario,
        index: usize,
        progress: Option<Arc<std::sync::Mutex<crate::model::simulation::RunProgress>>>,
    ) {
        let put_role = self.runner_put.clone();
        let sim_role = self.runner_sim.clone();
        let judge_role = self.judge.clone();
        let predicate = predicate.clone();
        let budget = budget.clone();
        let put_template = put.template.clone();
        let put_tools = put.tools.clone();
        tasks.spawn(async move {
            let runner = Runner::new(
                put_role.client,
                &put_role.model,
                sim_role.client,
                &sim_role.model,
            );
            let judge = Judge::new(judge_role.client, &judge_role.model);

            // A lightweight PUT view for the runner.
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
            let v = match judge.evaluate(&trace, &predicate, Some(&scenario)).await {
                Ok(v) => v,
                Err(e) => return RunOne::Failed(index, scenario, "judge", e.to_string()),
            };
            let matched = v.matched;
            let mut t = trace;
            t.verdict = Some(v);
            RunOne::Done(index, scenario, t, matched)
        });
    }
}
