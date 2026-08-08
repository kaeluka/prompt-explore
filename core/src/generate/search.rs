//! The search loop: ties hypothesize → build → run → judge into a
//! budget-bounded investigation that produces a RunResult.
//!
//! Existential mode (v1): find ONE trace that satisfies the criterion.
//! Scenarios run concurrently up to a configurable limit; the search
//! stops as soon as a witness is found.

use std::sync::Arc;

use tokio::task::JoinSet;

use crate::judge::Judge;
use crate::llm::LlmClient;
use crate::model::input::{Investigation, PromptUnderTest};
use crate::model::output::{Attribution, RunResult, RunStatus, Witness};
use crate::model::predicate::{Predicate, SuccessMode};
use crate::model::simulation::Scenario;
use crate::simulate::Runner;

use super::hypothesize::Hypothesizer;
use super::propose::ProposalGenerator;
use super::scenario::ScenarioBuilder;

/// One LLM client + model name, reused across generator roles.
#[derive(Clone)]
pub struct LlmRole {
    pub client: Arc<dyn LlmClient>,
    pub model: String,
}

pub struct Investigator {
    pub hypothesizer: LlmRole,
    pub builder: LlmRole,
    pub runner_put: LlmRole,
    pub runner_sim: LlmRole,
    pub judge: LlmRole,
    pub proposer: LlmRole,
    /// Max scenarios generated per hypothesis.
    pub scenarios_per_hypothesis: usize,
    /// Max hypotheses to generate.
    pub max_hypotheses: usize,
}

pub struct InvestigateOutcome {
    pub result: RunResult,
    /// All scenarios that were actually tried (for inspection/debugging).
    pub scenarios: Vec<Scenario>,
    /// Every completed run: scenario + trace + verdict. Essential for
    /// auditing negative results ("did it even try?") and spotting
    /// near-misses.
    pub attempts: Vec<Attempt>,
}

pub struct Attempt {
    pub scenario: Scenario,
    pub trace: crate::model::simulation::Trace,
    /// Whether the judge matched this trace. Denormalised from the
    /// trace's verdict for convenience; only populated by the
    /// explicit-scenarios path.
    #[allow(dead_code)]
    pub matched: bool,
}

impl Investigator {
    pub async fn investigate(
        &self,
        investigation: &Investigation,
        put: &PromptUnderTest,
    ) -> InvestigateOutcome {
        // The question IS the criterion — no operationalization step.
        let predicate = Predicate {
            criterion: investigation.question.clone(),
            success_mode: SuccessMode::Witness,
        };

        let hypothesizer =
            Hypothesizer::new(self.hypothesizer.client.clone(), &self.hypothesizer.model);
        let builder = ScenarioBuilder::new(self.builder.client.clone(), &self.builder.model);

        // 1. Hypothesize.
        let hypotheses = match hypothesizer
            .hypothesize(&investigation.question, put, self.max_hypotheses, None)
            .await
        {
            Ok(h) => h,
            Err(e) => return self.fail(format!("hypothesis generation failed: {e}")),
        };

        let mut strategies_tried: Vec<String> =
            hypotheses.iter().map(|h| h.claim.clone()).collect();
        let mut all_scenarios = Vec::new();
        let mut attempts = Vec::new();
        let mut budget_remaining = investigation.budget.max_scenarios as usize;

        // 2. For each hypothesis, build + run + judge scenarios.
        'outer: for hypothesis in &hypotheses {
            if budget_remaining == 0 {
                break;
            }
            let want = self.scenarios_per_hypothesis.min(budget_remaining);
            let scenarios = match builder
                .build(
                    hypothesis,
                    put,
                    want,
                    investigation.initial_state.as_deref(),
                    None,
                    investigation.budget.max_steps_per_trace,
                )
                .await
            {
                Ok(s) => s,
                Err(e) => {
                    strategies_tried.push(format!("[build error for {}] {e}", hypothesis.id));
                    continue;
                }
            };
            all_scenarios.extend(scenarios.clone());

            // Run + judge concurrently. Each task owns cloned Arcs.
            let mut tasks: JoinSet<Option<(Scenario, crate::model::simulation::Trace, bool)>> =
                JoinSet::new();
            for scenario in scenarios {
                self.spawn_run(&mut tasks, &predicate, &investigation.budget, put, scenario);
            }

            while let Some(res) = tasks.join_next().await {
                budget_remaining = budget_remaining.saturating_sub(1);
                if let Ok(Some((scenario, trace, matched))) = res {
                    attempts.push(Attempt {
                        scenario,
                        trace: trace.clone(),
                        matched,
                    });
                    if matched {
                        let witness = Witness {
                            attribution: Attribution {
                                instruction_spans: hypothesis.target_instructions.clone(),
                                evidence: format!(
                                    "heuristic attribution from hypothesis '{}': {}",
                                    hypothesis.id, hypothesis.claim
                                ),
                            },
                            traces: vec![trace.clone()],
                        };

                        // Step 3: propose (unverified) fixes.
                        let proposals = ProposalGenerator::new(
                            self.proposer.client.clone(),
                            &self.proposer.model,
                        )
                        .propose(
                            put,
                            &witness.attribution,
                            &crate::judge::render_transcript(&trace),
                            all_scenarios.last(),
                        )
                        .await
                        .unwrap_or_default();

                        return InvestigateOutcome {
                            result: RunResult {
                                status: RunStatus::WitnessFound,
                                scenarios_tried: investigation.budget.max_scenarios
                                    - budget_remaining as u32,
                                strategies_tried,
                                witness: Some(witness),
                                incidental_findings: vec![],
                                proposals,
                                final_state: Some(trace.final_world_state.clone()),
                            },
                            scenarios: all_scenarios,
                            attempts,
                        };
                    }
                }
                if budget_remaining == 0 {
                    break 'outer;
                }
            }
        }

        InvestigateOutcome {
            result: RunResult {
                status: RunStatus::NoWitnessWithinBudget,
                scenarios_tried: investigation.budget.max_scenarios - budget_remaining as u32,
                strategies_tried,
                witness: None,
                incidental_findings: vec![],
                proposals: vec![],
                final_state: attempts.last().map(|a| a.trace.final_world_state.clone()),
            },
            scenarios: all_scenarios,
            attempts,
        }
    }

    /// Run exactly the given scenarios against the PUT (no hypothesis/
    /// scenario generation). All provided scenarios are run — an explicit
    /// list is a contract; the search stops early only in `investigate`.
    pub async fn investigate_scenarios(
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

    /// Spawn one run+judge task for a scenario. Shared by both entry
    /// points so the trace machinery stays identical.
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

    fn fail(&self, msg: String) -> InvestigateOutcome {
        InvestigateOutcome {
            result: RunResult {
                status: RunStatus::Error,
                scenarios_tried: 0,
                strategies_tried: vec![msg],
                witness: None,
                incidental_findings: vec![],
                proposals: vec![],
                final_state: None,
            },
            scenarios: vec![],
            attempts: vec![],
        }
    }
}
