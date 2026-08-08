//! The evaluation loop: run explicit scenarios against a PUT, judge each
//! trace, and on a witness propose (unverified) fixes.
//!
//! There is no scenario generation here. Scenarios are authored outside
//! the harness (by the operator's agent) and passed in; an explicit list
//! is a contract — all of them are run.

use std::sync::Arc;

use tokio::task::JoinSet;

use crate::judge::Judge;
use crate::llm::LlmClient;
use crate::model::input::{Investigation, PromptUnderTest};
use crate::model::output::{Attribution, RunResult, RunStatus, Witness};
use crate::model::predicate::{Predicate, SuccessMode};
use crate::model::simulation::Scenario;
use crate::simulate::Runner;

use super::propose::ProposalGenerator;

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
    pub proposer: LlmRole,
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

impl Investigator {
    /// Run exactly the given scenarios against the PUT. All of them —
    /// an explicit list is a contract.
    pub async fn investigate(
        &self,
        investigation: &Investigation,
        put: &PromptUnderTest,
        scenarios: &[Scenario],
    ) -> InvestigateOutcome {
        let predicate = Predicate {
            criterion: investigation.question.clone(),
            success_mode: SuccessMode::Witness,
        };

        let mut tasks: JoinSet<Option<(Scenario, crate::model::simulation::Trace, bool)>> =
            JoinSet::new();
        for scenario in scenarios {
            self.spawn_run(
                &mut tasks,
                &predicate,
                &investigation.budget,
                put,
                scenario.clone(),
            );
        }

        let mut attempts = Vec::new();
        while let Some(res) = tasks.join_next().await {
            if let Ok(Some((scenario, trace, matched))) = res {
                attempts.push(Attempt {
                    scenario,
                    trace: trace.clone(),
                    matched,
                });
            }
        }

        let strategies_tried = scenarios
            .iter()
            .map(|s| format!("caller-provided scenario '{}'", s.id))
            .collect::<Vec<_>>();

        // Existential mode: the first matched attempt is the witness.
        if let Some(att) = attempts.iter().find(|a| a.matched) {
            let trace = &att.trace;
            let witness = Witness {
                attribution: Attribution {
                    instruction_spans: vec![],
                    evidence: format!("caller-provided scenario '{}'", att.scenario.id),
                },
                traces: vec![trace.clone()],
            };
            let proposals =
                ProposalGenerator::new(self.proposer.client.clone(), &self.proposer.model)
                    .propose(
                        put,
                        &witness.attribution,
                        &crate::judge::render_transcript(trace),
                        attempts.last().map(|a| &a.scenario),
                    )
                    .await
                    .unwrap_or_default();

            InvestigateOutcome {
                result: RunResult {
                    status: RunStatus::WitnessFound,
                    scenarios_tried: scenarios.len() as u32,
                    strategies_tried,
                    witness: Some(witness),
                    incidental_findings: vec![],
                    proposals,
                    final_state: Some(trace.final_world_state.clone()),
                },
                scenarios: scenarios.to_vec(),
                attempts,
            }
        } else {
            InvestigateOutcome {
                result: RunResult {
                    status: RunStatus::NoWitnessWithinBudget,
                    scenarios_tried: scenarios.len() as u32,
                    strategies_tried,
                    witness: None,
                    incidental_findings: vec![],
                    proposals: vec![],
                    final_state: attempts.last().map(|a| a.trace.final_world_state.clone()),
                },
                scenarios: scenarios.to_vec(),
                attempts,
            }
        }
    }

    /// Spawn one run+judge task for a scenario.
    fn spawn_run(
        &self,
        tasks: &mut JoinSet<Option<(Scenario, crate::model::simulation::Trace, bool)>>,
        predicate: &Predicate,
        budget: &crate::model::Budget,
        put: &PromptUnderTest,
        scenario: Scenario,
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
                input_vars: Default::default(),
                tools: put_tools,
                design_goals: String::new(),
            };
            let trace = runner.run(&put_view, &scenario, &budget).await.ok()?;
            let v = judge
                .evaluate(&trace, &predicate, Some(&scenario))
                .await
                .ok()?;
            let matched = v.matched;
            let mut t = trace;
            t.verdict = Some(v);
            Some((scenario, t, matched))
        });
    }
}
