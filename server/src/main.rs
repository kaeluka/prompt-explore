//! HTTP + web UI server for prompt-explore.
//!
//! Thin wrapper around the core library: no business logic lives here.
//! The core (`prompt-explore`) stays usable as a standalone lib/CLI.
//!
//! Investigations can run for minutes, so the API is job-based:
//! POST returns a job id immediately; clients poll for the result.
//! Job state is held in memory (lost on restart) — durable storage
//! is a deliberate v2 concern.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::{
    Router,
    extract::{Path, Request, State},
    http::{HeaderValue, StatusCode, header},
    middleware::{self, Next},
    response::{Html, IntoResponse, Json, Response},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use utoipa::OpenApi;
use uuid::Uuid;

use prompt_explore::generate::{Investigator, LlmRole, ProposalApplier};
use prompt_explore::llm::{OpenAiCompatibleClient, UsageTotals, UsageTracker};
use prompt_explore::model::input::{Investigation, PromptUnderTest};
use prompt_explore::model::output::{Proposal, RunResult};
use prompt_explore::model::simulation::{RunProgress, Scenario, TraceStep};
use serde_json::Value;

const MODEL: &str = "glm-5.2";

struct AppState {
    client: Option<Arc<OpenAiCompatibleClient>>,
    jobs: Mutex<HashMap<String, Job>>,
}

struct Job {
    status: JobStatus,
    result: Option<InvestigateResponse>,
    error: Option<String>,
    /// Live progress: populated as steps are simulated.
    progress: Arc<std::sync::Mutex<RunProgress>>,
    /// Wall-clock start, epoch millis.
    started_at: u64,
    /// The investigation question (the judge's criterion) — shown so a
    /// reader can judge the unfolding trace against it.
    question: String,
    /// The prompt under test.
    put: PromptUnderTest,
    /// The full input scenarios (narrative, world_state, simulator_notes),
    /// so the ground truth is visible while the run unfolds.
    scenarios: Vec<Scenario>,
}

#[derive(Clone, Copy, Serialize, PartialEq, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
enum JobStatus {
    Running,
    Done,
    Failed,
}

#[derive(Deserialize, utoipa::ToSchema)]
struct InvestigateRequest {
    investigation: Investigation,
    put: PromptUnderTest,
    /// Model for every LLM role (runner PUT + simulator, judge,
    /// proposer). Omit to use the server default (`glm-5.2`).
    #[serde(default)]
    model: Option<String>,
    /// The test cases to run. Required; ALL of them are run (an explicit
    /// list is a contract — the step/token budget applies per trace, not
    /// to the count). Scenarios are authored outside this API and are
    /// editable before running: reviewing them is the intended workflow.
    scenarios: Vec<Scenario>,
}

#[derive(Deserialize, utoipa::ToSchema)]
struct ApplyRequest {
    put: PromptUnderTest,
    proposal: Proposal,
}

#[derive(Serialize, utoipa::ToSchema)]
struct ApplyResponse {
    put: PromptUnderTest,
    template_diff: Vec<prompt_explore::generate::DiffPart>,
    goals_diff: Vec<prompt_explore::generate::DiffPart>,
}

#[derive(Serialize, Clone, utoipa::ToSchema)]
struct InvestigateResponse {
    result: RunResult,
    /// How many of the input scenarios completed a trace.
    scenarios_run: usize,
    /// The opening user message of the witness scenario, so the UI can
    /// show the full conversation (the trace steps start with the
    /// agent's first reply).
    witness_user_message: Option<String>,
    /// Every completed run — the evidence behind a negative result.
    attempts: Vec<AttemptView>,
    /// Cumulative token usage and call counts across the whole run.
    usage: UsageTotals,
}

#[derive(Serialize, Clone, utoipa::ToSchema)]
struct AttemptView {
    /// The scenario this attempt ran, by id — ties the evidence back
    /// to its world.
    scenario_id: String,
    user_message: Option<String>,
    hypothesis_id: String,
    matched: bool,
    verdict_rationale: String,
    verdict_confidence: Option<f32>,
    /// Structured steps, rendered as HTML by the UI.
    steps: Vec<TraceStep>,
    /// World state at the end of the trace (after all applied patches).
    final_world_state: HashMap<String, Value>,
    /// Number of tool calls the simulated PUT made in this trace.
    tool_calls: usize,
    /// The scenario's narrative (world spec), so the consumer can
    /// judge simulation quality alongside the trace.
    narrative: String,
}

#[derive(Serialize, utoipa::ToSchema)]
struct JobCreated {
    id: String,
}

#[derive(Serialize, Clone, utoipa::ToSchema)]
struct JobView {
    status: JobStatus,
    started_at: u64,
    /// The investigation question (the judge's criterion).
    question: String,
    /// The prompt under test.
    put: PromptUnderTest,
    /// The full input scenarios (narrative = ground truth, etc.).
    scenarios: Vec<Scenario>,
    /// Live progress — per-scenario state + steps simulated so far.
    /// Populated while running; frozen (all scenarios done/failed) when
    /// the job finishes. Lets a dashboard show a tool-call log as it
    /// happens.
    progress: RunProgress,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<InvestigateResponse>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Serialize, Clone, utoipa::ToSchema)]
struct JobSummary {
    id: String,
    status: JobStatus,
    started_at: u64,
    /// How many scenarios this job is running.
    scenarios: usize,
}

#[derive(utoipa::OpenApi)]
#[openapi(
    info(
        title = "prompt-explore API",
        version = env!("CARGO_PKG_VERSION"),
        description = "Property-based testing for agent behavior. You AUTHOR scenarios \
                       (test cases: a world specification plus a protagonist — see the \
                       Scenario schema) and submit them with a prompt under test (PUT) and \
                       a behavioral question. Every scenario is run: an LLM simulates the \
                       world from the scenario's narrative, the PUT acts in it, and a judge \
                       evaluates each resulting trace against your question. A witness is a \
                       trace where the questioned behavior actually occurred. Proposed prompt \
                       fixes are always unverified — apply them (POST /api/apply), then \
                       re-run the same scenarios to check. The API is job-based: POST returns \
                       a job id immediately; poll GET /api/investigations/{id} for the result."
    ),
    paths(index, list_investigations, create_investigation, get_investigation, apply_proposal)
)]
struct ApiDoc;

fn print_help() {
    println!("{} {}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
    println!();
    println!("Property-based testing for agent behavior. HTTP API + web UI.");
    println!();
    println!("USAGE:");
    println!("    {} [OPTIONS]", env!("CARGO_PKG_NAME"));
    println!();
    println!("OPTIONS:");
    println!("    --dump-openapi    Print the OpenAPI spec as JSON and exit");
    println!("    -h, --help        Print this help message and exit");
    println!();
    println!("ENVIRONMENT:");
    println!("    ZAI_API_KEY          Required. z.ai API key for LLM access.");
    println!("    PROMPT_EXPLORE_ADDR  Bind address (default: 127.0.0.1:8080).");
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();

    if args.iter().any(|a| a == "-h" || a == "--help") {
        print_help();
        return;
    }

    if args.iter().any(|a| a == "--dump-openapi") {
        println!(
            "{}",
            ApiDoc::openapi().to_pretty_json().expect("spec serializes")
        );
        return;
    }

    let key = std::env::var("ZAI_API_KEY").expect("set ZAI_API_KEY");
    let state = Arc::new(AppState {
        client: Some(Arc::new(OpenAiCompatibleClient::zai(&key))),
        jobs: Mutex::new(HashMap::new()),
    });

    let app = Router::new()
        .route("/", get(index))
        .route("/openapi.json", get(openapi_json))
        .route(
            "/api/investigations",
            get(list_investigations).post(create_investigation),
        )
        .route("/api/investigations/{id}", get(get_investigation))
        .route("/api/apply", post(apply_proposal))
        .route("/api/openapi.json", get(openapi_json))
        .layer(middleware::from_fn(spec_discovery))
        .with_state(state);

    let addr = std::env::var("PROMPT_EXPLORE_ADDR").unwrap_or_else(|_| "127.0.0.1:8080".into());
    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    eprintln!("prompt-explore server listening on http://{addr}");
    axum::serve(listener, app).await.unwrap();
}

async fn openapi_json() -> Json<utoipa::openapi::OpenApi> {
    Json(ApiDoc::openapi())
}

/// Discovery: every response advertises the OpenAPI spec
/// (machine-readable, `rel="service-desc"`) and the web UI
/// (human-readable, `rel="service-doc"`) via standard link relations,
/// so spec-aware tooling can find them from any endpoint.
async fn spec_discovery(req: Request, next: Next) -> Response {
    let mut res = next.run(req).await;
    res.headers_mut().insert(
        header::LINK,
        HeaderValue::from_static(r#"</openapi.json>; rel="service-desc", </>; rel="service-doc""#),
    );
    res
}

/// Serve the web UI.
#[utoipa::path(
    get,
    path = "/",
    responses((status = 200, description = "Web UI (HTML)", content_type = "text/html"))
)]
async fn index() -> impl axum::response::IntoResponse {
    // During development the UI changes often; in release builds the
    // embedded page is versioned with the binary, so normal caching
    // semantics are fine.
    if cfg!(debug_assertions) {
        (
            [(axum::http::header::CACHE_CONTROL, "no-cache")],
            Html(INDEX_HTML),
        )
            .into_response()
    } else {
        Html(INDEX_HTML).into_response()
    }
}

/// Start an investigation: run every given scenario against the PUT and
/// judge each trace against the question. Runs in the background; poll
/// the returned id. The result includes every attempt (scenario + trace
/// + verdict), any witness, unverified fix proposals, and token usage.
#[utoipa::path(
    post,
    path = "/api/investigations",
    request_body = InvestigateRequest,
    responses(
        (status = 202, description = "Investigation job created", body = JobCreated)
    )
)]
async fn create_investigation(
    State(state): State<Arc<AppState>>,
    Json(req): Json<InvestigateRequest>,
) -> (StatusCode, Json<JobCreated>) {
    let id = Uuid::new_v4().to_string();
    let progress = Arc::new(std::sync::Mutex::new(RunProgress::default()));
    let started_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    state.jobs.lock().unwrap().insert(
        id.clone(),
        Job {
            status: JobStatus::Running,
            result: None,
            error: None,
            progress: progress.clone(),
            started_at,
            question: req.investigation.question.clone(),
            put: req.put.clone(),
            scenarios: req.scenarios.clone(),
        },
    );

    let state2 = state.clone();
    let id2 = id.clone();
    let model = req.model.clone().unwrap_or_else(|| MODEL.into());
    tokio::spawn(async move {
        let inner = state2.client.as_ref().unwrap().clone();
        let tracker = Arc::new(UsageTracker::new(inner));
        let role = || LlmRole {
            client: tracker.clone(),
            model: model.clone(),
        };
        let investigator = Investigator {
            runner_put: role(),
            runner_sim: role(),
            judge: role(),
            proposer: role(),
        };

        let outcome = investigator
            .investigate(
                &req.investigation,
                &req.put,
                &req.scenarios,
                Some(progress.clone()),
            )
            .await;

        let witness_user_message = outcome
            .attempts
            .iter()
            .find(|a| a.trace.verdict.as_ref().is_some_and(|v| v.matched))
            .and_then(|a| a.scenario.user_message.clone());

        let attempts = outcome
            .attempts
            .iter()
            .map(|a| AttemptView {
                scenario_id: a.scenario.id.clone(),
                user_message: a.scenario.user_message.clone(),
                hypothesis_id: a.scenario.hypothesis_id.clone(),
                matched: a.trace.verdict.as_ref().map_or(false, |v| v.matched),
                verdict_rationale: a
                    .trace
                    .verdict
                    .as_ref()
                    .map(|v| v.rationale.clone())
                    .unwrap_or_default(),
                verdict_confidence: a.trace.verdict.as_ref().and_then(|v| v.confidence),
                steps: a.trace.steps.clone(),
                final_world_state: a.trace.final_world_state.clone(),
                tool_calls: a
                    .trace
                    .steps
                    .iter()
                    .filter(|s| s.tool_call.is_some())
                    .count(),
                narrative: a.scenario.narrative.clone(),
            })
            .collect();

        let mut jobs = state2.jobs.lock().unwrap();
        if let Some(job) = jobs.get_mut(&id2) {
            job.status = JobStatus::Done;
            job.result = Some(InvestigateResponse {
                result: outcome.result,
                scenarios_run: outcome.scenarios.len(),
                witness_user_message,
                attempts,
                usage: tracker.totals(),
            });
        }
    });

    (StatusCode::ACCEPTED, Json(JobCreated { id }))
}

/// List all jobs (for the dashboard). Running jobs first, then by
/// recency. Returns summaries only — poll a job's id for full progress.
#[utoipa::path(
    get,
    path = "/api/investigations",
    responses((status = 200, description = "All jobs", body = [JobSummary]))
)]
async fn list_investigations(State(state): State<Arc<AppState>>) -> Json<Vec<JobSummary>> {
    let jobs = state.jobs.lock().unwrap();
    let mut rows: Vec<JobSummary> = jobs
        .iter()
        .map(|(id, j)| JobSummary {
            id: id.clone(),
            status: j.status,
            started_at: j.started_at,
            scenarios: j.progress.lock().unwrap().scenarios.len(),
        })
        .collect();
    // Running first, then newest-started first.
    rows.sort_by(|a, b| {
        let ar = a.status == JobStatus::Running;
        let br = b.status == JobStatus::Running;
        br.cmp(&ar).then_with(|| b.started_at.cmp(&a.started_at))
    });
    Json(rows)
}

/// Poll an investigation job. `progress` is always present (live steps
/// while running, frozen when done); `result` is present once done.
#[utoipa::path(
    get,
    path = "/api/investigations/{id}",
    params(("id" = String, Path, description = "Job id returned by POST /api/investigations")),
    responses(
        (status = 200, description = "Job status + live progress (+ result when done)", body = JobView),
        (status = 404, description = "Unknown job id")
    )
)]
async fn get_investigation(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<JobView>, StatusCode> {
    let jobs = state.jobs.lock().unwrap();
    let job = jobs.get(&id).ok_or(StatusCode::NOT_FOUND)?;
    Ok(Json(JobView {
        status: job.status,
        started_at: job.started_at,
        question: job.question.clone(),
        put: job.put.clone(),
        scenarios: job.scenarios.clone(),
        progress: job.progress.lock().unwrap().clone(),
        result: job.result.clone(),
        error: job.error.clone(),
    }))
}

/// Apply a proposal: the LLM rewrites the target field (template, or
/// design goals for goal_revision), and a deterministic word-level diff
/// is returned for review alongside the updated prompt.
#[utoipa::path(
    post,
    path = "/api/apply",
    request_body = ApplyRequest,
    responses(
        (status = 200, description = "Updated prompt plus template/goals diffs", body = ApplyResponse),
        (status = 500, description = "LLM apply failed", body = String)
    )
)]
async fn apply_proposal(
    State(state): State<Arc<AppState>>,
    Json(req): Json<ApplyRequest>,
) -> Result<Json<ApplyResponse>, (StatusCode, String)> {
    let client = state.client.as_ref().unwrap().clone();
    let applier = ProposalApplier::new(client, MODEL);
    let applied = applier
        .apply(&req.put, &req.proposal)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(ApplyResponse {
        put: PromptUnderTest {
            template: applied.template,
            design_goals: applied.design_goals,
            ..req.put
        },
        template_diff: applied.template_diff,
        goals_diff: applied.goals_diff,
    }))
}

const INDEX_HTML: &str = include_str!("../static/index.html");
