//! An HTTP representation of existing evidence, not a summary or a verdict.
use super::*;

/// Preferred agent reading surface: complete execution without duplicate data.
/// This endpoint has NO `progress` or `result` wrapper. Use TOP-LEVEL `turns`
/// and `workflow.invocations` / `workflow.tool_calls`, including on failed or
/// running jobs. `progress.turns` belongs to GET /api/investigations/{id}, NOT
/// this /evidence endpoint. Invocation turn_start/turn_end index the top-level
/// turns array; invocations do not contain nested turns arrays.
/// Read every `turns[].tool_exchanges[].response`: this is what
/// the PUT actually observed. `workspace_ops` only shows what the simulator
/// consulted; `lua_execution.outcome=computed` only says code ran, not that the
/// reply was faithful. Compare replies with `scenario.world` AND the tool
/// contracts on `scenario.tools`.
/// An invalid root listing or false-empty search can invalidate a comparison even
/// when the final answer looks right. Source revisions may differ across reruns.
///
/// Available while running and after failure: successful exchanges and setup
/// artifacts are retained. `execution.stop_reason` distinguishes a final completion
/// from a budget cutoff or runtime failure; `status=done` alone does not. Full
/// responses, model/simulator reasoning, workspace operations and Lua source are
/// all preserved here. No semantic compression or inferred correctness flags.
///
/// Next action in a prompt-optimization loop: immediately SEND PATCH
/// /api/investigations/{id} with your evidence-based `grades` and `assessment`,
/// using the user's agreed rubric and zero-based turn/exchange references. Confirm
/// the response echoes the annotations; a local score or a planned PATCH is not a
/// recorded judgment. If simulation or evidence cannot justify a score, record the
/// limitation in assessment and leave/clear that grade rather than fabricate one.
/// Then POST /api/frontier with those quality axes and inspect `points` BEFORE
/// editing the prompt. A run missing a requested grade remains in the group's
/// explicit backlog, but cannot contribute coordinates on that comparison.
#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct InvestigationEvidence {
    id: String,
    status: JobStatus,
    phase: RunPhase,
    started_at: u64,
    #[schema(required = true)]
    finished_at: Option<u64>,
    budget: Budget,
    execution: RunExecution,
    reason: Option<String>,
    /// The submitted program, retained even if execution never started.
    workflow_program: prompt_explore::model::workflow::WorkflowProgram,
    /// Complete orchestration evidence: source/params/output, exact agent
    /// inputs and turn ranges, and direct tool calls with responses/provenance.
    /// Read this output rather than assuming the last agent turn was delivered.
    workflow: Option<prompt_explore::model::workflow::WorkflowEvidence>,
    scenario: Scenario,
    sim_model: String,
    sim_thinking_level: Option<ThinkingLevel>,
    conversation_controls: ResolvedConversationControls,
    workspace_files: usize,
    /// The reusable scenario this run pinned, with the exact revision and
    /// content hash that produced these traces.
    scenario_id: String,
    scenario_revision: u64,
    scenario_definition_hash: String,
    attributes: BTreeMap<String, String>,
    grades: BTreeMap<String, f64>,
    #[schema(required = true)]
    assessment: Option<Assessment>,
    user_message: Option<String>,
    resolved_inputs: HashMap<String, Value>,
    implementations: Vec<ToolImplementation>,
    /// Complete PUT model turns, each with tool arguments AND actual responses.
    /// EvidenceReference indices refer directly to this array and its exchanges.
    turns: Vec<TraceTurn>,
    /// Null until a trace completes, including budget-capped completion.
    final_world_state: Option<HashMap<String, Value>>,
    failure: Option<RunFailure>,
    /// Present on terminal jobs, including failures. Live usage is not estimated.
    usage: Option<UsageByRole>,
}

impl From<JobView> for InvestigationEvidence {
    fn from(job: JobView) -> Self {
        let workflow = job
            .result
            .as_ref()
            .and_then(|r| r.trace.as_ref())
            .and_then(|trace| trace.workflow.clone())
            .or_else(|| job.progress.workflow.clone());
        let (execution, turns, resolved_inputs, implementations, final_world_state, failure, usage) =
            match job.result {
                Some(result) => match result.trace {
                    Some(trace) => (
                        trace.execution,
                        trace.turns,
                        trace.resolved_inputs,
                        trace.implementations,
                        Some(trace.final_world_state),
                        result.failure,
                        Some(result.usage),
                    ),
                    None => (
                        job.progress.execution,
                        job.progress.turns,
                        job.progress.resolved_inputs,
                        job.progress.implementations,
                        None,
                        result.failure,
                        Some(result.usage),
                    ),
                },
                None => (
                    job.progress.execution,
                    job.progress.turns,
                    job.progress.resolved_inputs,
                    job.progress.implementations,
                    None,
                    None,
                    None,
                ),
            };
        Self {
            id: job.id,
            status: job.status,
            phase: job.phase,
            started_at: job.started_at,
            finished_at: job.finished_at,
            budget: job.budget,
            execution,
            reason: job.reason,
            workflow_program: job.workflow,
            workflow,
            scenario: job.scenario,
            sim_model: job.sim_model,
            sim_thinking_level: job.sim_thinking_level,
            conversation_controls: job.conversation_controls,
            workspace_files: job.workspace_files,
            scenario_id: job.scenario_id,
            scenario_revision: job.scenario_revision,
            scenario_definition_hash: job.scenario_definition_hash,
            attributes: job.attributes,
            grades: job.grades,
            assessment: job.assessment,
            user_message: job.progress.user_message,
            resolved_inputs,
            implementations,
            turns,
            final_world_state,
            failure,
            usage,
        }
    }
}

/// Read one complete conversation, not a final-answer/usage-only projection.
/// Prefer this endpoint for judging and archiving: it removes duplicated terminal
/// progress without dropping any tool responses or provenance. Inspect execution
/// and every call/response before grading. A plausible final answer can conceal a
/// faulty simulator. If evidence is inadequate, record that limitation in an
/// assessment rather than inventing a fidelity score; sharpen the world/tool
/// contract or change simulator settings and submit a separate investigation.
/// Then PATCH your own grades and assessment, and explicitly group the variables
/// being compared at POST /api/frontier. The harness never performs this judgment.
#[utoipa::path(
    get,
    path = "/api/investigations/{id}/evidence",
    params(("id" = String, Path, description = "Investigation id")),
    security(("api_token" = [])),
    responses(
        (status = 200, description = "Nonduplicated complete evidence, live or terminal, including partial failure evidence", body = InvestigationEvidence),
        (status = 401, description = "Missing or invalid bearer token"),
        (status = 404, description = "Unknown investigation")
    )
)]
pub(super) async fn get_evidence(
    state: State<Arc<AppState>>,
    id: Path<String>,
) -> Result<Json<InvestigationEvidence>, StatusCode> {
    let Json(job) = get_investigation(state, id).await?;
    Ok(Json(job.into()))
}
