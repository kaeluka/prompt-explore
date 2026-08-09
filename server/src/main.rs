//! HTTP + web UI server for prompt-explore.
//!
//! Thin wrapper around the core library: no business logic lives here.
//! The core (`prompt-explore`) stays usable as a standalone lib/CLI.
//!
//! Investigations can run for minutes, so the API is job-based:
//! POST returns a job id immediately; clients poll for the result.
//! Job state is held in memory (lost on restart) — durable storage
//! is a deliberate v2 concern.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

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
use prompt_explore::llm::{list_all_map, ProviderClient, ProviderModels, UsageTotals, UsageTracker};
use prompt_explore::model::input::{Investigation, PromptUnderTest};
use prompt_explore::model::output::{Proposal, RunResult};
use prompt_explore::model::simulation::{RunProgress, Scenario, TraceStep};
use serde_json::Value;

const MODEL: &str = "glm-5.2";

struct AppState {
    client: Option<Arc<ProviderClient>>,
    jobs: Mutex<HashMap<String, Job>>,
    /// The effective default provider (PROMPT_EXPLORE_PROVIDER), surfaced
    /// by GET /api/models so callers know what a bare model name resolves to.
    default_provider: String,
    /// LLM model listing (GET /models) — genai client + a short-TTL cache
    /// so repeated listing doesn't hammer the providers.
    models_client: prompt_explore::llm::GenaiClient,
    models_cache: Mutex<Option<(Instant, ModelsResponse)>>,
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
    /// The original request (question, budget, put, scenarios, per-role
    /// models) — lets POST /api/investigations/{id}/apply re-run the
    /// identical experiment against a rewritten PUT without the caller
    /// echoing everything back.
    request: InvestigateRequest,
}

#[derive(Clone, Copy, Serialize, PartialEq, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
enum JobStatus {
    Running,
    Done,
    Failed,
}

#[derive(Deserialize, Clone, utoipa::ToSchema)]
struct InvestigateRequest {
    investigation: Investigation,
    put: PromptUnderTest,
    /// Model for every LLM role (runner PUT + judge + proposer). Omit to
    /// use the server default (`glm-5.2`). Provider is selected by
    /// namespace prefix, e.g. `zai_coding::glm-5.2`,
    /// `open_router::deepseek/...`, `bedrock_sigv4::<model-id>`; a bare
    /// name uses the server's default provider (`PROMPT_EXPLORE_PROVIDER`).
    /// See `GET /api/models` for available namespaced model strings.
    ///
    /// This is the model you are TESTING: when experimenting to find
    /// which model works well for your prompt, this is the one you vary
    /// across runs. Keep `sim_model` fixed while you do (see below), so
    /// each candidate PUT is judged in the same simulated environment.
    #[serde(default)]
    model: Option<String>,
    /// Model for the tool SIMULATOR only (the LLM that roleplays the
    /// environment). Defaults to `model`.
    ///
    /// The simulator is the test ENVIRONMENT, not the thing under test.
    /// Two consequences:
    /// 1. When tuning which model works well for your prompt, keep
    ///    `sim_model` STABLE across runs (vary `model`, not this). You
    ///    are comparing candidate PUTs; the environment must stay fixed
    ///    so differences in the traces come from the PUT, not from a
    ///    shifting simulation.
    /// 2. The simulator must be POWERFUL ENOUGH to render a believable
    ///    environment — a weak simulator produces inconsistent or
    ///    unbelievable tool responses, which corrupts every trace
    ///    regardless of how good the PUT is. There is a quality floor
    ///    below which results stop being meaningful, even if it's
    ///    cheaper. Pick a strong model here and leave it set.
    #[serde(default)]
    sim_model: Option<String>,
    /// Model for the JUDGE only (the LLM that evaluates each trace against
    /// your question). Defaults to `model`. The judge is the
    /// safety-critical role: a weak judge fails to catch what a weak PUT
    /// does, so it should be at least as strong as the PUT, ideally
    /// stronger. Splitting it out lets you keep a strong judge while you
    /// vary the PUT model — and a stronger judge also catches simulator
    /// divergence (tool responses that contradict the narrative).
    #[serde(default)]
    judge_model: Option<String>,
    /// Model for the PROPOSER only (the LLM that suggests prompt fixes
    /// from a witness). Defaults to `model`. Lower priority than the
    /// judge; proposals are always unverified anyway.
    #[serde(default)]
    proposer_model: Option<String>,
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

/// Apply a proposal generated by a finished job to that job's PUT and
/// re-run the SAME scenarios against the rewritten PUT — closing the
/// edit/test loop the spec tells users to walk ("apply, then re-run to
/// check") without echoing the whole request back.
#[derive(Deserialize, utoipa::ToSchema)]
struct ApplyToJobRequest {
    /// The id of a proposal in this job's `result.proposals`.
    proposal_id: String,
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
                       a job id immediately; poll GET /api/investigations/{id} for the result.

 \
                       DESIGN INTENT — why it works this way:
 \
                       • Scenarios are world SPECIFICATIONS, not instantiated data. A \
                       narrative pins what exists (inventory; facts, including NEGATIVE \
                       facts; completeness assertions; rendering rules) and the simulator \
                       lazily renders concrete tool responses from it. Materializing a full \
                       environment requires a closed world (enumerable, bounded, copyable); \
                       open worlds — web search, email, a payment network — can never be \
                       materialized, so a narrative (prose) is the only mechanism that \
                       generalizes. This is why a scenario is a spec, not a fixture.
 \
                       • Tool responses are SIMULATED by an LLM from the narrative, not \
                       scripted. Deterministic / pinned responses (e.g. a `when_called_with` \
                       override) are a deliberate NON-GOAL: any fixture or DSL you build \
                       fails to express a realistic case, and making the harness own \
                       simulation fidelity just swaps LLM flakiness (already accepted) for \
                       harness bugs (now your problem). `example_responses` are realism \
                       hints for the simulator, NOT pinned outputs.
 \
                       • The answer to simulation unreliability is TRANSPARENCY, not \
                       enforcement. Every tool response is in the trace; the judge sees the \
                       same narrative and can flag a response that contradicts the stated \
                       facts. Divergence is SURFACED for you to read, not silently fixed.
 \
                       • Because tool responses are LLM-simulated, an investigation MAY \
                       contain unrealistic or WRONG results — responses that contradict the \
                       narrative, invent facts, or drift across calls. The harness does NOT \
                       vet them. It is the CALLER'S responsibility to read the traces and \
                       double-check the simulated tool responses thoroughly before trusting \
                       any verdict. When simulation quality is insufficient, iterate with \
                       three levers and re-run the same scenarios: (a) sharpen the scenario \
                       NARRATIVE — tighter facts and negative facts; (b) use a stronger \
                       SIM_MODEL — it must be powerful enough to simulate believably; (c) use \
                       a stronger JUDGE_MODEL — so divergence is caught."
    ),
    paths(index, list_investigations, create_investigation, get_investigation, apply_proposal, apply_to_job, list_models)
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
    println!("    PROMPT_EXPLORE_PROVIDER  Which provider runs the LLM calls (default: zai).");
    println!("                           zai | zai_standard | openrouter | bedrock");
    println!("    ZAI_API_KEY            API key for zai / zai_standard (coding-plan default).");
    println!("    OPENROUTER_API_KEY     API key for openrouter.");
    println!("    bedrock uses the default AWS credential chain (aws sso login, profiles, IMDS).");
    println!("    PROMPT_EXPLORE_ADDR    Bind address (default: 0.0.0.0:8080, LAN-reachable).");
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

    let provider = std::env::var("PROMPT_EXPLORE_PROVIDER").unwrap_or_else(|_| "zai".into());
    let client = match provider.as_str() {
        "zai" => ProviderClient::zai(),
        "zai_standard" => ProviderClient::zai_standard(),
        "openrouter" => ProviderClient::openrouter(),
        "bedrock" => ProviderClient::bedrock(),
        other => panic!("unknown PROMPT_EXPLORE_PROVIDER '{other}' (zai | zai_standard | openrouter | bedrock)"),
    };
    let state = Arc::new(AppState {
        client: Some(Arc::new(client)),
        jobs: Mutex::new(HashMap::new()),
        default_provider: provider.clone(),
        models_client: prompt_explore::llm::GenaiClient::builder().build(),
        models_cache: Mutex::new(None),
    });

    let app = Router::new()
        .route("/", get(index))
        .route("/openapi.json", get(openapi_json))
        .route("/vendor/preact.mjs", get(vendor_preact))
        .route("/vendor/hooks.mjs", get(vendor_hooks))
        .route("/vendor/htm.mjs", get(vendor_htm))
        .route(
            "/api/investigations",
            get(list_investigations).post(create_investigation),
        )
        .route("/api/investigations/{id}", get(get_investigation))
        .route("/api/investigations/{id}/apply", post(apply_to_job))
        .route("/api/apply", post(apply_proposal))
        .route("/api/models", get(list_models))
        .route("/api/openapi.json", get(openapi_json))
        .layer(middleware::from_fn(spec_discovery))
        .with_state(state);

    let addr = std::env::var("PROMPT_EXPLORE_ADDR").unwrap_or_else(|_| "0.0.0.0:8080".into());
    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    eprintln!("prompt-explore server listening on http://{addr}");
    axum::serve(listener, app).await.unwrap();
}

async fn openapi_json() -> Json<utoipa::openapi::OpenApi> {
    Json(ApiDoc::openapi())
}

/// Models available to put in a request's `model` field, by provider.
///
/// Returns the server defaults plus a map keyed by provider namespace
/// (`zai_coding`, `open_router`, `bedrock_sigv4`). Each provider value is
/// either `{available: {models: [{name, pricing?}]}}` — where `name` is the
/// full pastable, namespaced string (e.g.
/// `open_router::deepseek/deepseek-v4-flash-0731`) — or `{error: "…"}`
/// explaining why that provider couldn't be listed (no API key in the
/// environment, no AWS credentials, region-gated, …). Listing is
/// best-effort and per-provider: one provider failing never breaks the
/// others. Cached for a short time so repeated listing is cheap.
#[derive(Serialize, Clone, utoipa::ToSchema)]
struct ModelsResponse {
    /// Model used when a request omits `model` (a bare name; the server
    /// resolves it via `server_default_provider`).
    server_default_model: String,
    /// Provider applied to bare model names when no namespace is given
    /// (from PROMPT_EXPLORE_PROVIDER). Maps to a namespace prefix:
    /// `zai` -> `zai_coding::`, `zai_standard` -> `zai::`,
    /// `openrouter` -> `open_router::`, `bedrock` -> `bedrock_sigv4::`.
    server_default_provider: String,
    providers: BTreeMap<String, ProviderModels>,
}

#[utoipa::path(
    get,
    path = "/api/models",
    tag = "models",
    responses((status = 200, description = "Available models per provider", body = ModelsResponse))
)]
async fn list_models(
    State(state): State<Arc<AppState>>,
) -> Json<ModelsResponse> {
    const TTL: Duration = Duration::from_secs(60);
    if let Some((fetched_at, cached)) = state.models_cache.lock().unwrap().clone() {
        if fetched_at.elapsed() < TTL && cached.server_default_provider == state.default_provider {
            return Json(cached);
        }
    }
    let providers = list_all_map(&state.models_client).await;
    let resp = ModelsResponse {
        server_default_model: MODEL.into(),
        server_default_provider: state.default_provider.clone(),
        providers,
    };
    *state.models_cache.lock().unwrap() = Some((Instant::now(), resp.clone()));
    Json(resp)
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
    let id = spawn_investigation(state, req);
    (StatusCode::ACCEPTED, Json(JobCreated { id }))
}

/// Create a job for `req`, spawn its run, and return the job id. Shared
/// by POST /api/investigations (fresh runs) and POST
/// /api/investigations/{id}/apply (re-run with a rewritten PUT).
fn spawn_investigation(state: Arc<AppState>, req: InvestigateRequest) -> String {
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
            request: req.clone(),
        },
    );

    let state2 = state.clone();
    let id2 = id.clone();
    let put_model = req.model.clone().unwrap_or_else(|| MODEL.into());
    let sim_model = req.sim_model.clone().unwrap_or_else(|| put_model.clone());
    let judge_model = req.judge_model.clone().unwrap_or_else(|| put_model.clone());
    let proposer_model = req.proposer_model.clone().unwrap_or_else(|| put_model.clone());
    tokio::spawn(async move {
        let inner = state2.client.as_ref().unwrap().clone();
        let tracker = Arc::new(UsageTracker::new(inner));
        let investigator = Investigator {
            runner_put: LlmRole { client: tracker.clone(), model: put_model.clone() },
            runner_sim: LlmRole { client: tracker.clone(), model: sim_model },
            judge: LlmRole { client: tracker.clone(), model: judge_model },
            proposer: LlmRole { client: tracker.clone(), model: proposer_model },
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

    id
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

#[utoipa::path(
    post,
    path = "/api/investigations/{id}/apply",
    tag = "investigations",
    request_body = ApplyToJobRequest,
    params(("id" = String, Path, description = "Job id whose proposals to apply")),
    responses(
        (status = 202, description = "A new job re-running the same scenarios against the rewritten PUT", body = JobCreated),
        (status = 404, description = "Job not found"),
        (status = 409, description = "Job has no result yet, or no proposals (no witness)"),
        (status = 500, description = "LLM apply failed", body = String)
    )
)]
async fn apply_to_job(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(req): Json<ApplyToJobRequest>,
) -> Result<(StatusCode, Json<JobCreated>), (StatusCode, String)> {
    // Pull what we need out of the job under the lock, then apply (async)
    // and re-run outside the lock.
    let (request, put, proposal) = {
        let jobs = state.jobs.lock().unwrap();
        let job = jobs
            .get(&id)
            .ok_or((StatusCode::NOT_FOUND, format!("job {id} not found")))?;
        let result = job
            .result
            .as_ref()
            .ok_or((StatusCode::CONFLICT, "job has no result yet".to_string()))?;
        let proposal = result
            .result
            .proposals
            .iter()
            .find(|p| p.id == req.proposal_id)
            .cloned()
            .ok_or((
                StatusCode::CONFLICT,
                format!(
                    "no proposal with id '{}' (job has {} proposals)",
                    req.proposal_id,
                    result.result.proposals.len()
                ),
            ))?;
        (job.request.clone(), job.put.clone(), proposal)
    };

    // The rewrite runs on the job's proposer model if set, else its PUT
    // model, else the server default — not a hardcoded default, so a
    // capped default provider doesn't block applying a proposal.
    let applier_model = request
        .proposer_model
        .clone()
        .or_else(|| request.model.clone())
        .unwrap_or_else(|| MODEL.into());
    let client = state.client.as_ref().unwrap().clone();
    let applier = ProposalApplier::new(client, applier_model);
    let applied = applier
        .apply(&put, &proposal)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // Same experiment, rewritten PUT.
    let mut new_req = request;
    new_req.put = PromptUnderTest {
        template: applied.template,
        design_goals: applied.design_goals,
        ..put
    };

    let new_id = spawn_investigation(state, new_req);
    Ok((StatusCode::ACCEPTED, Json(JobCreated { id: new_id })))
}

const INDEX_HTML: &str = include_str!("../static/index.html");
const VENDOR_PREACT: &str = include_str!("../static/vendor/preact.mjs");
const VENDOR_HOOKS: &str = include_str!("../static/vendor/hooks.mjs");
const VENDOR_HTM: &str = include_str!("../static/vendor/htm.mjs");

async fn vendor_preact() -> impl axum::response::IntoResponse {
    ([("content-type", "text/javascript;charset=utf-8")], VENDOR_PREACT)
}
async fn vendor_hooks() -> impl axum::response::IntoResponse {
    ([("content-type", "text/javascript;charset=utf-8")], VENDOR_HOOKS)
}
async fn vendor_htm() -> impl axum::response::IntoResponse {
    ([("content-type", "text/javascript;charset=utf-8")], VENDOR_HTM)
}

#[cfg(test)]
mod tests {
    use super::INDEX_HTML;

    #[test]
    fn openapi_spec_is_discoverable_from_root_body() {
        // WHY THIS EXISTS: spec discovery used to be header-only
        // (a `Link: rel="service-desc"` header on every response). That
        // is the RFC 8631 standard and it is correct — but it is invisible
        // to agents/tools that read only the response BODY. When a body-
        // only consumer hit "/" it got an HTML page with no reference to
        // the spec anywhere, and could not discover it (observed: an
        // agent pasted http://host/ and found nothing). The "/" body now
        // carries a `<link rel="service-desc" href="/openapi.json">` tag
        // in <head> (plus a visible footer line) so the spec is
        // discoverable from the body itself, not just the header. This
        // test guards against that marker being silently removed —
        // removing it re-breaks body-only consumers, which is easy to do
        // by accident since the header still works and hides the regression.
        assert!(
            INDEX_HTML.contains(r#"rel="service-desc" href="/openapi.json""#),
            "the / body must advertise the OpenAPI spec via a service-desc \
             link tag; body-only consumers (most agent HTTP tools) cannot \
             see the Link header"
        );
    }
}
