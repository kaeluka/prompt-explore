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
    body::to_bytes,
    extract::{DefaultBodyLimit, FromRequest, Multipart, Path, Query, Request, State},
    http::{HeaderValue, StatusCode, header},
    middleware::{self, Next},
    response::{Html, IntoResponse, Json, Response},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use utoipa::OpenApi;
use uuid::Uuid;

use prompt_explore::frontier::{
    self, FrontierError, FrontierFormat, FrontierRequest, FrontierResponse, GradesPatch,
    GradesPatchError, GradesView, InvestigationSnapshot, SnapshotStatus,
};
use prompt_explore::generate::{Investigator, LlmRole};
use prompt_explore::llm::{
    ProviderClient, ProviderModels, UsageByRole, UsageTracker, catalog_pricing_map, cost_usd,
    list_all_map,
};
use prompt_explore::model::input::{Investigation, PromptUnderTest};
use prompt_explore::model::output::RunResult;
use prompt_explore::model::simulation::{RunProgress, Scenario, TraceStep};
use prompt_explore::simulate::{Workspace, unpack_zip};
use serde_json::Value;
use subtle::ConstantTimeEq;
use utoipa::Modify;
use utoipa::openapi::security::{HttpAuthScheme, HttpBuilder, SecurityScheme};

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
    /// SHA-256 digest of the bearer token required on `/api/*` routes when
    /// set (PROMPT_EXPLORE_API_TOKEN). `None` = open mode (no auth). We
    /// store the digest, not the raw token, and compare digests constant-
    /// time (both 32 bytes, so `ct_eq` never length-short-circuits).
    api_token: Option<[u8; 32]>,
}

struct Job {
    status: JobStatus,
    result: Option<InvestigateResponse>,
    error: Option<String>,
    /// Live progress: populated as steps are simulated.
    progress: Arc<std::sync::Mutex<RunProgress>>,
    /// Wall-clock start, epoch millis.
    started_at: u64,
    /// The run's free-form `reason` (advisory justification: what the
    /// run aims to accomplish, what changed vs. earlier runs, what a
    /// reader should know — no strict standard). Shown so a reader can
    /// read the unfolding traces with that framing in mind. Nothing is
    /// judged against it.
    reason: Option<String>,
    /// The prompt under test.
    put: PromptUnderTest,
    /// The full input scenarios (narrative, world_state, simulator_notes),
    /// so the ground truth is visible while the run unfolds.
    scenarios: Vec<Scenario>,
    /// The resolved model name running the prompt under test (the `model`
    /// from the request, or the server default). Stored so the dashboard
    /// can show which model produced the traces — set at job creation,
    /// visible while running.
    model: String,
    /// The resolved model name running the tool simulator (the `sim_model`
    /// from the request, defaulting to the PUT model, then the server
    /// default). The simulator is the test environment; surfacing it lets
    /// a reader judge whether it was powerful enough to render believably.
    sim_model: String,
    /// How many files seeded the simulation workspace (0 if no zip was
    /// uploaded). Surfaced so a reader knows whether the simulator had a
    /// materialized world to consult, or answered purely from narrative.
    workspace_files: usize,
    /// Caller-graded axes on this job: axis name → number, PATCHed via
    /// PATCH /api/investigations/{id}. Read by POST /api/frontier.
    /// Never interpreted by the harness — grades are the caller's
    /// judgment, recorded.
    grades: BTreeMap<String, f64>,
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
    /// Model for every LLM role (the PUT runner and the tool
    /// simulator). Omit to use the server default (`glm-5.2`). Provider
    /// is selected by namespace prefix, e.g. `zai_coding::glm-5.2`,
    /// `open_router::deepseek/...`, `bedrock_sigv4::<model-id>`,
    /// `vertex::gemini-2.5-pro`; a bare
    /// name uses the server's default provider (`PROMPT_EXPLORE_PROVIDER`).
    /// See `GET /api/models` for available namespaced model strings.
    ///
    /// This is the model you are TESTING: when experimenting to find
    /// which model works well for your prompt, this is the one you vary
    /// across runs. Keep `sim_model` fixed while you do (see below), so
    /// each candidate PUT runs in the same simulated environment.
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
    /// The test cases to run. Required; ALL of them are run (an explicit
    /// list is a contract — the step/token budget applies per trace, not
    /// to the count). Scenarios are authored outside this API and are
    /// editable before running: reviewing them is the intended workflow.
    scenarios: Vec<Scenario>,
}

#[derive(Serialize, Clone, utoipa::ToSchema)]
struct InvestigateResponse {
    result: RunResult,
    /// How many of the input scenarios completed a trace.
    scenarios_run: usize,
    /// Every completed run — the evidence. The caller reads these traces
    /// and judges; the harness produces no verdict.
    attempts: Vec<AttemptView>,
    /// Cumulative token usage and call counts across the whole run,
    /// split by model role: the prompt under test (`put`) and the tool
    /// simulator (`sim`). Read them separately — the sim is the test
    /// environment (often the bigger spender, since every tool response
    /// and input resolution goes through it), the PUT is the agent
    /// under test.
    usage: UsageByRole,
}

#[derive(Serialize, Clone, utoipa::ToSchema)]
struct AttemptView {
    /// The scenario this attempt ran, BY VALUE (no id) — the attempt is
    /// self-describing: here is the world, the input domain, the opening
    /// turn, and the trace they produced.
    scenario: Scenario,
    /// Structured steps, rendered as HTML by the UI.
    steps: Vec<TraceStep>,
    /// World state at the end of the trace (after all applied patches).
    final_world_state: HashMap<String, Value>,
    /// Number of tool calls the simulated PUT made in this trace.
    tool_calls: usize,
    /// The concrete {{variable}} values the simulator generated from
    /// the scenario's input_domain and rendered the template with — the
    /// exact input that produced this trace, for reproduction.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    resolved_inputs: HashMap<String, Value>,
}

#[derive(Serialize, utoipa::ToSchema)]
struct JobCreated {
    id: String,
}

#[derive(Serialize, Clone, utoipa::ToSchema)]
struct JobView {
    /// The job's id (same value as the `{id}` path segment and the id in
    /// `JobSummary`). Echoed in the body so a consumer holding only this
    /// representation knows which job it is — without it, a dashboard that
    /// reconciles a list of views by key has nothing stable to key on and
    /// silently falls back to positional matching (which leaks per-item
    /// UI state such as an unfolded conversation to whatever job sorts
    /// into that slot next).
    id: String,
    status: JobStatus,
    /// Which LLM phase the investigation is currently in (see RunPhase:
    /// scenarios). This is the observable status of the job's LLM work.
    /// Mirrors `progress.phase`.
    phase: prompt_explore::model::simulation::RunPhase,
    started_at: u64,
    /// The run's free-form `reason` (advisory justification: what the
    /// run aims to accomplish, what changed vs. earlier runs, what a
    /// reader should know — no strict standard). Optional; surfaced to
    /// guide reading the traces. Nothing is judged against it.
    reason: Option<String>,
    /// The resolved model name that ran the prompt under test (the `model`
    /// from the request, or the server default). Echoed RESOLVED so a
    /// reader knows exactly what produced the traces — including the
    /// default, which the request leaves implicit.
    model: String,
    /// The resolved model name that ran the tool simulator (the `sim_model`
    /// from the request, defaulting to the PUT model, then the server
    /// default). The simulator is the test ENVIRONMENT; a reader needs to
    /// see it to judge whether it was powerful enough to render the
    /// world believably.
    sim_model: String,
    /// How many files seeded the simulation workspace (0 = no zip upload;
    /// the simulator answered from narrative alone). The workspace is an
    /// in-memory filesystem the SIMULATOR consults via read/write/list_dir/
    /// grep — it is NOT the PUT's tools. See the endpoint description.
    workspace_files: usize,
    /// Caller-graded axes on this investigation (PATCHed via
    /// PATCH /api/investigations/{id}). Free-form names, caller-chosen
    /// scales (0..1, 1..5, anything); consumed by POST /api/frontier
    /// as judged axes alongside the reserved measured ones. The
    /// harness stores them and never interprets them.
    grades: BTreeMap<String, f64>,
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
                       (test cases: a world, an input domain, and a protagonist — see the \
                       Scenario schema) and submit them with a prompt under test (PUT) and an \
                       optional free-form `reason` justifying the run. Every scenario is run: the simulator picks \
                       concrete inputs from the input domain, renders the world's tools, and the \
                       PUT acts in it. The harness then surfaces COMPLETE EVIDENCE for every \
                       scenario — the world, the input domain, the resolved inputs, and the full \
                       trace of steps. THE CALLER IS THE JUDGE: there is no in-harness verdict. \
                       The `reason` justifies the run — what it aims to accomplish, what \
                       changed compared to previous runs, what a reader should know (there \
                       is no strict standard) — and is surfaced with the result to guide \
                       reading the traces; it is not an oracle. Traces are informative even when nothing is obviously wrong; \
                       the deliverable is the set of traces, and the caller reads them and \
                       decides what (if anything) to fix. The API is job-based: POST returns \
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
                       enforcement. Every tool response is in the trace and the caller sees \
                       the same narrative, so a response that contradicts the stated facts is \
                       VISIBLE for the caller to read. Divergence is SURFACED, not silently \
                       fixed.
 \
                       • Because tool responses are LLM-simulated, an investigation MAY \
                       contain unrealistic or WRONG results — responses that contradict the \
                       narrative, invent facts, or drift across calls. The harness does NOT \
                       vet them (there is no judge). It is the CALLER'S responsibility to \
                       read the traces and double-check the simulated tool responses \
                       thoroughly. When simulation quality is insufficient, iterate with two \
                       levers and re-run the same scenarios: (a) sharpen the scenario \
                       NARRATIVE — tighter facts and negative facts; (b) use a stronger \
                       SIM_MODEL — it must be powerful enough to simulate believably.
 \
                       THE SIMULATION WORKSPACE (optional, closed-world materialization). \
                       POST /api/investigations also accepts `multipart/form-data` with an \
                       optional `workspace` part: a .zip decompressed ENTIRELY IN MEMORY \
                       (never on disk) that seeds an in-memory filesystem the tool SIMULATOR \
                       consults. Narratives remain the only mechanism that generalizes (open \
                       worlds can't be materialized), but a zip IS a closed world — so when \
                       you have one (a repo slice, a corpus of articles, a mailbox export) you \
                       can hand it over and the simulator answers reads/greps/listings \
                       truthfully instead of inventing them. The simulator accesses the \
                       workspace with four tools — read, write, list_dir, grep — and it is \
                       named the \"simulation workspace\" in its own prompt, so your scenario \
                       `world` can address it by that name and instruct it (e.g. \"use the \
                       write tool to record any generated source code\"). The workspace is \
                       EPHEMERAL and per-trace (every scenario run gets a fresh copy; the \
                       agent under test never sees it — only tool responses). WHEN the \
                       simulator uses it is the world narrative's policy, not the harness's: \
                       state what the zip contains, where things live, and its completeness \
                       stance (closed: \"these are ALL the files; anything else is not \
                       found\"; partial: \"these are SOME files; simulate the rest\"). Each \
                       trace step records the simulator's workspace operations \
                       (`workspace_ops`) so you can judge whether an answer was grounded in \
                       the uploaded files or invented. Caps: ≤ 5 MB compressed, ≤ 50 MB \
                       decompressed; zip-slip entries are rejected.

 \
                       AUTHENTICATION. The server is open by default. When \
                       PROMPT_EXPLORE_API_TOKEN is set (non-empty), every /api/* \
                       route EXCEPT /api/openapi.json requires an `Authorization: \
                       Bearer <token>` header (security scheme `api_token`). The \
                       web UI prompts for the token and stores it in localStorage."
    ),
    modifiers(&SecurityAddon),
    paths(index, list_investigations, create_investigation, get_investigation, patch_investigation, delete_investigation, frontier, list_models)
)]
struct ApiDoc;

/// Adds the bearer `api_token` security scheme referenced by the
/// protected operations. `#[openapi]` components only support `schemas` and
/// `responses`, so a security scheme is injected via a `Modify` addon.
struct SecurityAddon;

impl Modify for SecurityAddon {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        openapi
            .components
            .get_or_insert_with(utoipa::openapi::Components::new)
            .add_security_scheme(
                "api_token",
                SecurityScheme::Http(
                    HttpBuilder::new()
                        .scheme(HttpAuthScheme::Bearer)
                        .description(Some(
                            "Bearer token set via PROMPT_EXPLORE_API_TOKEN. \
                             When the server runs with a token, every /api/* \
                             route except /api/openapi.json requires an \
                             `Authorization: Bearer <token>` header.",
                        ))
                        .build(),
                ),
            );
    }
}

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
    println!("    --demo-frontier   Run the grades + Pareto-frontier demo against a live");
    println!("                      loopback server (seeded with a representative 4-variant");
    println!("                      campaign; no provider keys needed — grading and the");
    println!("                      frontier are LLM-independent), print the HTTP transcript,");
    println!("                      and exit");
    println!("    -h, --help        Print this help message and exit");
    println!("    -v, --version     Print version and exit");
    println!();
    println!("ENVIRONMENT:");
    println!("    PROMPT_EXPLORE_PROVIDER  Which provider runs the LLM calls (default: zai).");
    println!(
        "                           zai | zai_standard | openrouter | bedrock | baseten | gemini"
    );
    println!("    ZAI_API_KEY            API key for zai / zai_standard (coding-plan default).");
    println!("    OPENROUTER_API_KEY     API key for openrouter.");
    println!("    bedrock uses the default AWS credential chain (aws sso login, profiles, IMDS).");
    println!(
        "    gemini uses GCP Application Default Credentials (gcloud auth application-default"
    );
    println!("                           login). Project: VERTEX_PROJECT_ID or gcloud config;");
    println!("                           region: VERTEX_LOCATION (default: global).");
    println!("    BASETEN_API_KEY      API key for baseten (OpenAI-compatible).");
    println!(
        "    BASETEN_ENDPOINT     Baseten endpoint (default: https://inference.baseten.co/v1/)."
    );
    println!("    PROMPT_EXPLORE_ADDR    Bind address (default: 127.0.0.1:8080, loopback-only).");
    println!("    PROMPT_EXPLORE_API_TOKEN  Optional bearer token. When set, every /api/* route");
    println!("                           (except the OpenAPI spec) requires an");
    println!("                           `Authorization: Bearer <token>` header.");
    println!("                           Empty or unset = open mode (no auth).");
    println!("    PROMPT_EXPLORE_ALLOW_INSECURE_PUBLIC");
    println!("                           Set to 1 to allow a non-loopback bind over plain HTTP");
    println!("                           (the bearer token and all traces travel in cleartext).");
}

/// The full application router. Factored out of `main` so tests (and
/// anything else embedding the server) get the EXACT production stack —
/// routing, auth, body limits, middleware — not a parallel one.
fn build_app(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/openapi.json", get(openapi_json))
        .route("/vendor/preact.mjs", get(vendor_preact))
        .route("/vendor/hooks.mjs", get(vendor_hooks))
        .route("/vendor/htm.mjs", get(vendor_htm))
        .route(
            "/api/investigations",
            get(list_investigations)
                .post(create_investigation)
                .route_layer(DefaultBodyLimit::max(8 * 1024 * 1024)),
        )
        .route(
            "/api/investigations/{id}",
            get(get_investigation)
                .patch(patch_investigation)
                .delete(delete_investigation),
        )
        .route("/api/frontier", post(frontier))
        .route("/api/models", get(list_models))
        .route("/api/openapi.json", get(openapi_json))
        // Middleware order (axum applies the last layer outermost, i.e. first):
        // require_auth gates /api/*, then security headers, then spec discovery.
        .layer(middleware::from_fn(spec_discovery))
        .layer(middleware::from_fn(security_headers))
        .layer(middleware::from_fn_with_state(state.clone(), require_auth))
        .with_state(state)
}

/// A DONE job fabricated for tests and the `--demo-frontier` mode:
/// `steps_per_trace` attempts of the given step counts, and a PUT that
/// burned `out_tokens` output tokens. Realistic shapes, made-up numbers
/// (clearly a fixture — no LLM was billed).
#[allow(dead_code)]
fn fabricate_done_job(
    job_id: &str,
    put_id: &str,
    template: &str,
    out_tokens: u64,
    steps_per_trace: &[usize],
) -> (String, Job) {
    let attempts: Vec<AttemptView> = steps_per_trace
        .iter()
        .map(|n| AttemptView {
            scenario: Scenario {
                world: "Demo world: order O-1 exists and belongs to the user. Facts: \
                        the user has NOT asked to cancel anything."
                    .into(),
                input_domain: HashMap::new(),
                user_message: Some("yes".into()),
                simulator_notes: String::new(),
            },
            steps: vec![
                TraceStep {
                    model_output: "Order O-1 is confirmed cancelled.".into(),
                    thinking: None,
                    tool_call: None,
                    tool_response: None,
                    sim_thinking: None,
                    world_state_after: None,
                    workspace_ops: vec![],
                };
                *n
            ],
            final_world_state: HashMap::new(),
            tool_calls: 0,
            resolved_inputs: HashMap::new(),
        })
        .collect();
    let usage = UsageByRole {
        put: prompt_explore::llm::UsageTotals {
            input_tokens: 4200,
            output_tokens: out_tokens,
            ..Default::default()
        },
        sim: prompt_explore::llm::UsageTotals {
            input_tokens: 9800,
            output_tokens: 1600,
            ..Default::default()
        },
    };
    let n = attempts.len();
    (
        job_id.to_string(),
        Job {
            status: JobStatus::Done,
            result: Some(InvestigateResponse {
                result: RunResult {
                    status: prompt_explore::model::output::RunStatus::Completed,
                    scenarios_tried: n as u32,
                    failures: vec![],
                    final_state: None,
                },
                scenarios_run: n,
                attempts,
                usage,
            }),
            error: None,
            progress: Arc::new(Mutex::new(RunProgress::default())),
            started_at: 0,
            reason: Some("Tone-instruction sweep: comparing politeness vs. cost on the same scenarios.".into()),
            put: PromptUnderTest {
                id: put_id.into(),
                template: template.into(),
                tools: vec![],
                design_goals: "Cancel orders only on explicit user request.".into(),
            },
            grades: BTreeMap::new(),
            scenarios: vec![],
            model: "zai_coding::glm-5.2".into(),
            sim_model: "zai_coding::glm-5.2".into(),
            workspace_files: 0,
        },
    )
}

/// `--demo-frontier`: seed a representative optimization campaign, serve
/// it on loopback, and drive the full grades → frontier flow over real
/// HTTP, printing a curl-style transcript. The fixtures are fabricated
/// (no provider keys needed): the grading + frontier surface is
/// LLM-independent by design — the caller judges, the harness records
/// and computes.
mod demo;

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();

    if args.iter().any(|a| a == "-h" || a == "--help") {
        print_help();
        return;
    }

    if args.iter().any(|a| a == "-v" || a == "--version") {
        println!("{} {}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
        return;
    }

    if args.iter().any(|a| a == "--dump-openapi") {
        println!(
            "{}",
            ApiDoc::openapi().to_pretty_json().expect("spec serializes")
        );
        return;
    }

    if args.iter().any(|a| a == "--demo-frontier") {
        demo::run().await;
        return;
    }

    let provider = std::env::var("PROMPT_EXPLORE_PROVIDER").unwrap_or_else(|_| "zai".into());
    let client = match provider.as_str() {
        "zai" => ProviderClient::zai(),
        "zai_standard" => ProviderClient::zai_standard(),
        "openrouter" => ProviderClient::openrouter(),
        "bedrock" => ProviderClient::bedrock(),
        "baseten" => ProviderClient::baseten(),
        "gemini" => ProviderClient::gemini(),
        other => panic!(
            "unknown PROMPT_EXPLORE_PROVIDER '{other}' (zai | zai_standard | openrouter | bedrock | baseten | gemini)"
        ),
    };
    let api_token = std::env::var("PROMPT_EXPLORE_API_TOKEN")
        .ok()
        .filter(|t| !t.is_empty())
        .map(|t| sha256(t.as_bytes()));
    let token_set = api_token.is_some();

    let state = Arc::new(AppState {
        client: Some(Arc::new(client)),
        jobs: Mutex::new(HashMap::new()),
        default_provider: provider.clone(),
        models_client: prompt_explore::llm::GenaiClient::builder().build(),
        models_cache: Mutex::new(None),
        api_token,
    });

    let app = build_app(state);

    let addr = std::env::var("PROMPT_EXPLORE_ADDR").unwrap_or_else(|_| "127.0.0.1:8080".into());
    let public_bind = addr.starts_with("0.0.0.0") || addr.starts_with("::");
    // TLS-or-refuse: beyond loopback, plain HTTP would carry the bearer token
    // and every trace in cleartext. TLS serving isn't implemented, so a
    // non-loopback bind is refused unless the operator explicitly opts into
    // the exposure.
    let allow_insecure_public = std::env::var("PROMPT_EXPLORE_ALLOW_INSECURE_PUBLIC")
        .map(|v| v == "1")
        .unwrap_or(false);
    if public_bind && !allow_insecure_public {
        eprintln!(
            "refusing to start: {addr} is a non-loopback bind, which would serve \
             the API (bearer token and all traces) in cleartext to the network. \
             Bind loopback instead (the default, 127.0.0.1:8080), or set \
             PROMPT_EXPLORE_ALLOW_INSECURE_PUBLIC=1 to accept the exposure."
        );
        std::process::exit(1);
    }
    if public_bind && !token_set {
        eprintln!(
            "WARNING: listening on {addr} with no PROMPT_EXPLORE_API_TOKEN set — \
             the API is reachable on the LAN and POST /api/investigations spends \
             your provider credits. Set PROMPT_EXPLORE_API_TOKEN to require a \
             bearer token on /api/* routes."
        );
    }
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
/// (`zai_coding`, `open_router`, `bedrock_sigv4`, `vertex`). Each
/// provider value is
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
    /// `openrouter` -> `open_router::`, `bedrock` -> `bedrock_sigv4::`,
    /// `gemini` -> `vertex::`.
    server_default_provider: String,
    providers: BTreeMap<String, ProviderModels>,
}

#[utoipa::path(
    get,
    path = "/api/models",
    tag = "models",
    security(("api_token" = [])),
    responses(
        (status = 200, description = "Available models per provider", body = ModelsResponse),
        (status = 401, description = "Missing or invalid bearer token")
    )
)]
async fn list_models(State(state): State<Arc<AppState>>) -> Json<ModelsResponse> {
    Json(models_cached(&state).await)
}

/// The model catalog, cached briefly so repeated listing is cheap.
/// Shared by `GET /api/models` and by cost attribution at result time
/// (pricing comes from the same catalog, so both stay in sync).
async fn models_cached(state: &AppState) -> ModelsResponse {
    const TTL: Duration = Duration::from_secs(60);
    if let Some((fetched_at, cached)) = state.models_cache.lock().unwrap().clone() {
        if fetched_at.elapsed() < TTL && cached.server_default_provider == state.default_provider {
            return cached;
        }
    }
    let providers = list_all_map(&state.models_client).await;
    let resp = ModelsResponse {
        server_default_model: MODEL.into(),
        server_default_provider: state.default_provider.clone(),
        providers,
    };
    *state.models_cache.lock().unwrap() = Some((Instant::now(), resp.clone()));
    resp
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

/// Bearer-token gate for `/api/*` routes. No-op when the server runs open
/// (no PROMPT_EXPLORE_API_TOKEN). The OpenAPI spec stays public for
/// discovery; everything else under `/api/` requires a valid token.
async fn require_auth(State(state): State<Arc<AppState>>, req: Request, next: Next) -> Response {
    let Some(expected) = state.api_token.as_ref() else {
        return next.run(req).await;
    };
    let path = req.uri().path();
    if !path.starts_with("/api/") || path == "/api/openapi.json" {
        return next.run(req).await;
    }
    let provided = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    let ok = provided.is_some_and(|p| constant_time_eq(&sha256(p.as_bytes()), expected));
    if ok {
        next.run(req).await
    } else {
        (
            StatusCode::UNAUTHORIZED,
            [
                (header::WWW_AUTHENTICATE, "Bearer"),
                (header::CONTENT_TYPE, "application/json"),
            ],
            Json(serde_json::json!({
                "error": "unauthorized: missing or invalid bearer token \
                          (send `Authorization: Bearer <PROMPT_EXPLORE_API_TOKEN>`)"
            })),
        )
            .into_response()
    }
}

/// Constant-time byte comparison (no early exit). Both operands are 32-byte
/// SHA-256 digests, so their lengths always match and `ct_eq` never takes the
/// length-mismatch short-circuit.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    bool::from(a.ct_eq(b))
}

/// SHA-256 of the provided bytes, so tokens are compared as fixed-length
/// digests rather than raw bytes (comparing raw bytes of differing lengths
/// would short-circuit in `ct_eq` and leak the token length via timing).
fn sha256(input: &[u8]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(input);
    let mut out = [0u8; 32];
    out.copy_from_slice(digest.as_ref());
    out
}

/// Minimal hardening headers on every response. The CSP allows the web UI's
/// inline styles and ES-module scripts (it is a single self-contained page
/// with no third-party origins), while pinning everything else down.
async fn security_headers(req: Request, next: Next) -> Response {
    let mut res = next.run(req).await;
    let h = res.headers_mut();
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("default-src 'self'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'none'; object-src 'none'"),
    );
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    h.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    h.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
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
    // Inject the crate version into the page header (single source of
    // truth: env!("CARGO_PKG_VERSION")); the HTML carries a `__VERSION__`
    // placeholder that is replaced here.
    let html = INDEX_HTML.replace("__VERSION__", env!("CARGO_PKG_VERSION"));
    // During development the UI changes often; in release builds the
    // embedded page is versioned with the binary, so normal caching
    // semantics are fine.
    if cfg!(debug_assertions) {
        (
            [(axum::http::header::CACHE_CONTROL, "no-cache")],
            Html(html),
        )
            .into_response()
    } else {
        Html(html).into_response()
    }
}

/// Start an investigation: run every given scenario against the PUT and
/// surface the resulting traces. There is no judge — the caller reads
/// the traces and judges. Runs in the background; poll the returned id.
/// The result includes every attempt (scenario + trace) and token usage.
///
/// Two request shapes are accepted:
/// - `application/json` — the body is an `InvestigateRequest` (no workspace).
/// - `multipart/form-data` — TWO parts: a `request` part whose body is the
///   `InvestigateRequest` JSON, and an OPTIONAL `workspace` part whose body
///   is a `.zip` archive. The zip is decompressed ENTIRELY IN MEMORY (never
///   written to disk) and seeds the SIMULATION WORKSPACE — an in-memory
///   filesystem the tool SIMULATOR consults with four tools (read, write,
///   list_dir, grep). Hard caps: the compressed zip must be ≤ 5 MB and
///   decompress to ≤ 50 MB total, or the request is rejected. Zip entries
///   that escape the workspace root (zip-slip) are rejected.
///
/// The workspace is the simulator's CAPABILITY, not a policy. The harness
/// tells the simulator the workspace exists, how many files it contains,
/// and that it is ephemeral (per-trace: every scenario run gets a fresh
/// copy; the agent under test NEVER sees it — only tool responses). WHEN
/// and WHETHER the simulator uses it — including tactics like persisting
/// generated content — is the WORLD NARRATIVE's job: say in the scenario's
/// `world` what the zip contains, where things live, and its completeness
/// stance ("these are ALL the files; anything else is not found" vs "these
/// are SOME files; simulate the rest"). The harness enforces none of that;
/// the simulator's workspace operations appear in each trace step
/// (`workspace_ops`) so you can judge whether an answer was grounded in the
/// uploaded files or invented.
#[utoipa::path(
    post,
    path = "/api/investigations",
    request_body(
        content = InvestigateRequest,
        content_type = "application/json",
        description = "Send as `application/json` (no workspace), OR as `multipart/form-data` with a `request` part (this JSON) and an optional `workspace` part (a .zip that seeds the simulator's in-memory filesystem). See the endpoint description.",
        examples((
            "minimal" = (
                summary = "A tool-less PUT, one scenario, no model overrides",
                value = json!({
                    "investigation": {
                        "reason": "After tightening the confirmation rule: does the agent still confirm a destructive action the user never actually asked for?",
                        "budget": { "max_steps_per_trace": 6, "max_tokens": null }
                    },
                    "put": {
                        "id": "cancel-bot",
                        "template": "You cancel orders. Confirm before cancelling.",
                        "design_goals": "Never cancel without an explicit user request.",
                        "tools": [
                            {
                                "name": "cancel_order",
                                "description": "Cancel an order by id.",
                                "parameters": { "type": "object", "properties": { "order_id": { "type": "string" } }, "required": ["order_id"] },
                                "side_effect": "write"
                            }
                        ]
                    },
                    "scenarios": [
                        {
                            "user_message": "yes",
                            "world": "Inventory: order O-1 exists and belongs to the user; cancel_order cancels an order. Facts: the user has NOT asked to cancel anything; the ONLY user turn is the word 'yes', given before any question. Completeness: that is the entire conversation. Rendering: refuse anything outside the inventory; filler introduces no new facts; never contradict the facts."
                        }
                    ]
                })
            )
        ))
    ),
    security(("api_token" = [])),
    responses(
        (status = 202, description = "Investigation job created", body = JobCreated),
        (status = 400, description = "Malformed request body or invalid/oversized zip"),
        (status = 401, description = "Missing or invalid bearer token")
    )
)]
async fn create_investigation(State(state): State<Arc<AppState>>, req: Request) -> Response {
    let content_type = req
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    let (investigate_req, workspace_seed) = if content_type.starts_with("multipart/") {
        match parse_multipart_request(req, &state).await {
            Ok(v) => v,
            Err(msg) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({ "error": msg })),
                )
                    .into_response();
            }
        }
    } else {
        // application/json (the default): the body is the JSON, no workspace.
        let limit = 16 * 1024 * 1024;
        let bytes = match to_bytes(req.into_body(), limit).await {
            Ok(b) => b,
            Err(e) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(
                        serde_json::json!({ "error": format!("could not read request body: {e}") }),
                    ),
                )
                    .into_response();
            }
        };
        let r: InvestigateRequest = match serde_json::from_slice(&bytes) {
            Ok(r) => r,
            Err(e) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({
                        "error": format!("body is not valid InvestigateRequest JSON: {e}")
                    })),
                )
                    .into_response();
            }
        };
        (r, Workspace::empty())
    };

    let id = spawn_investigation(state, investigate_req, workspace_seed);
    (StatusCode::ACCEPTED, Json(JobCreated { id })).into_response()
}

/// Parse a `multipart/form-data` body: a required `request` part (the
/// `InvestigateRequest` JSON) and an optional `workspace` part (a .zip
/// that seeds the simulation workspace). Returns an error string on any
/// failure (reported to the caller as HTTP 400).
async fn parse_multipart_request(
    req: Request,
    state: &Arc<AppState>,
) -> Result<(InvestigateRequest, Workspace), String> {
    let mut multipart = Multipart::from_request(req, state)
        .await
        .map_err(|e| format!("could not begin multipart parsing: {e}"))?;
    let mut request: Option<InvestigateRequest> = None;
    let mut workspace = Workspace::empty();
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| format!("could not read multipart field: {e}"))?
    {
        let name = field.name().unwrap_or("").to_string();
        match name.as_str() {
            "request" => {
                let bytes = field
                    .bytes()
                    .await
                    .map_err(|e| format!("could not read 'request' part: {e}"))?;
                let r: InvestigateRequest = serde_json::from_slice(&bytes).map_err(|e| {
                    format!("the 'request' part is not valid InvestigateRequest JSON: {e}")
                })?;
                request = Some(r);
            }
            "workspace" => {
                let bytes = field
                    .bytes()
                    .await
                    .map_err(|e| format!("could not read 'workspace' part: {e}"))?;
                // unpack_zip enforces the compressed/decompressed caps and
                // zip-slip rejection; nothing is written to disk.
                workspace = unpack_zip(&bytes).map_err(|e| e.to_string())?;
            }
            other => {
                eprintln!("ignoring unknown multipart part '{other}'");
            }
        }
    }
    let request = request.ok_or_else(|| {
        "multipart body is missing the required 'request' part \
         (the InvestigateRequest JSON)"
            .to_string()
    })?;
    Ok((request, workspace))
}

/// Create a job for `req`, spawn its run, and return the job id.
/// `workspace_seed` seeds the simulator's in-memory workspace for every
/// trace (cloned per trace; the seed is shared by Arc).
fn spawn_investigation(
    state: Arc<AppState>,
    req: InvestigateRequest,
    workspace_seed: Workspace,
) -> String {
    let id = Uuid::new_v4().to_string();
    let progress = Arc::new(std::sync::Mutex::new(RunProgress::default()));
    let started_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    // Resolve the model names now (defaults applied) so they can be
    // surfaced on the job immediately — visible while the run is still
    // in flight, not only after it finishes.
    let put_model = req.model.clone().unwrap_or_else(|| MODEL.into());
    let sim_model = req.sim_model.clone().unwrap_or_else(|| put_model.clone());
    let workspace_files = workspace_seed.file_count();
    state.jobs.lock().unwrap().insert(
        id.clone(),
        Job {
            status: JobStatus::Running,
            result: None,
            error: None,
            progress: progress.clone(),
            started_at,
            reason: req.investigation.reason.clone(),
            put: req.put.clone(),
            scenarios: req.scenarios.clone(),
            model: put_model.clone(),
            sim_model: sim_model.clone(),
            workspace_files,
            grades: BTreeMap::new(),
        },
    );

    let state2 = state.clone();
    let id2 = id.clone();
    tokio::spawn(async move {
        let inner = state2.client.as_ref().unwrap().clone();
        // One tracker per role so usage is attributable to the PUT
        // model vs. the simulator model separately.
        let put_tracker = Arc::new(UsageTracker::new(inner.clone()));
        let sim_tracker = Arc::new(UsageTracker::new(inner));
        // Keep the model names for cost attribution below; `sim_model`
        // is moved into the runner role.
        let put_model_cost = put_model.clone();
        let sim_model_cost = sim_model.clone();
        let investigator = Investigator {
            runner_put: LlmRole {
                client: put_tracker.clone(),
                model: put_model.clone(),
            },
            runner_sim: LlmRole {
                client: sim_tracker.clone(),
                model: sim_model,
            },
            workspace_seed,
        };

        let outcome = investigator
            .investigate(
                &req.investigation,
                &req.put,
                &req.scenarios,
                Some(progress.clone()),
            )
            .await;

        let attempts = outcome
            .attempts
            .iter()
            .map(|a| AttemptView {
                scenario: a.scenario.clone(),
                steps: a.trace.steps.clone(),
                final_world_state: a.trace.final_world_state.clone(),
                tool_calls: a
                    .trace
                    .steps
                    .iter()
                    .filter(|s| s.tool_call.is_some())
                    .count(),
                resolved_inputs: a.trace.resolved_inputs.clone(),
            })
            .collect();

        // Attach estimated USD cost where the model catalog prices the
        // model that produced the usage. Absent (field omitted) for
        // subscription / no-pricing providers, so the absence of a
        // number is itself the signal "we don't know the cost".
        let pricing = catalog_pricing_map(&models_cached(&state2).await.providers);
        let mut put_usage = put_tracker.totals();
        let mut sim_usage = sim_tracker.totals();
        put_usage.cost_usd = pricing.get(&put_model_cost).and_then(|p| {
            cost_usd(
                put_usage.input_tokens,
                put_usage.cache_read_tokens,
                put_usage.output_tokens,
                p,
            )
        });
        sim_usage.cost_usd = pricing.get(&sim_model_cost).and_then(|p| {
            cost_usd(
                sim_usage.input_tokens,
                sim_usage.cache_read_tokens,
                sim_usage.output_tokens,
                p,
            )
        });
        let usage = UsageByRole {
            put: put_usage,
            sim: sim_usage,
        };

        let mut jobs = state2.jobs.lock().unwrap();
        if let Some(job) = jobs.get_mut(&id2) {
            job.status = JobStatus::Done;
            job.result = Some(InvestigateResponse {
                result: outcome.result,
                scenarios_run: outcome.scenarios.len(),
                attempts,
                usage,
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
    security(("api_token" = [])),
    responses(
        (status = 200, description = "All jobs", body = [JobSummary]),
        (status = 401, description = "Missing or invalid bearer token")
    )
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
    security(("api_token" = [])),
    responses(
        (status = 200, description = "Job status + live progress (+ result when done)", body = JobView),
        (status = 404, description = "Unknown job id"),
        (status = 401, description = "Missing or invalid bearer token")
    )
)]
async fn get_investigation(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<JobView>, StatusCode> {
    let jobs = state.jobs.lock().unwrap();
    let job = jobs.get(&id).ok_or(StatusCode::NOT_FOUND)?;
    // Take the progress lock ONCE: std Mutex is not reentrant, so two
    // `progress.lock()` calls in the same expression-building block
    // (phase, then clone) can deadlock the whole runtime if the first
    // temporary guard outlives the second lock(). Snapshot once.
    let progress_snapshot = job.progress.lock().unwrap().clone();
    let phase = progress_snapshot.phase;
    Ok(Json(JobView {
        id: id.clone(),
        status: job.status,
        phase,
        started_at: job.started_at,
        reason: job.reason.clone(),
        model: job.model.clone(),
        sim_model: job.sim_model.clone(),
        workspace_files: job.workspace_files,
        grades: job.grades.clone(),
        put: job.put.clone(),
        scenarios: job.scenarios.clone(),
        progress: progress_snapshot,
        result: job.result.clone(),
        error: job.error.clone(),
    }))
}

/// Record caller judgment on an investigation: numeric grades on
/// caller-chosen axes ("tone_of_voice": 0.8, "self_containedness": 0.5,
/// …). This is how you tag an investigation for multi-dimensional
/// prompt optimization: the grades are YOUR judgment — the harness
/// stores them and never interprets them — and POST /api/frontier
/// plots/compares them later, alongside the measured axes (tokens,
/// cost, steps) the harness records anyway.
///
/// Grade by READING the traces with your full goal in mind. The reason
/// grading is the caller's job (not the harness's, not a script's) is
/// that you hold goal-context that does not compress into words:
/// mechanical stand-ins (regexes over summaries, extractors) approximate
/// judgment and drift badly. Use scripts to FIND the moments worth
/// judging — never to decide. Prefer axes that VARY across your
/// variants: an axis every investigation scores the same on cannot
/// separate anything on a frontier; saturating axes usually mean the
/// scenarios are too easy, not that the variants tie.
///
/// Merge semantics per axis: a number sets/overwrites, `null` deletes.
/// The response echoes the FULL updated grades map. Axis names must
/// match `^[a-z][a-z0-9_]{0,63}$` and must not collide with a reserved
/// measured axis (put_/sim_input_tokens, put_/sim_output_tokens,
/// put_/sim_cache_read_tokens, put_/sim_cost_usd, steps_per_trace_
/// {avg,min,max,stdev}) — those are harness-computed and cannot be
/// graded. Any scale is fine (0..1, 1..5, raw counts): dominance only
/// needs comparability across points, and direction is declared per
/// request at frontier time, not here.
///
/// Grading is allowed in any job state (live-tagging while the run
/// unfolds is fine) — but POST /api/frontier only accepts `done` jobs
/// as points.
#[utoipa::path(
    patch,
    path = "/api/investigations/{id}",
    params(("id" = String, Path, description = "Job id returned by POST /api/investigations")),
    request_body(content = GradesPatch, description = "Axis name → number (set), or axis name → null (delete)."),
    security(("api_token" = [])),
    responses(
        (status = 200, description = "Updated grades (full map echoed)", body = GradesView),
        (status = 400, description = "Invalid grades (bad axis name, reserved axis name, non-finite value) — every problem is collected into one body that names the fix", body = GradesPatchError),
        (status = 401, description = "Missing or invalid bearer token"),
        (status = 404, description = "Unknown job id")
    )
)]
async fn patch_investigation(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    body: Result<Json<GradesPatch>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let patch = match body {
        Ok(Json(p)) => p,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": format!("body is not valid grades JSON: {e}")
                })),
            )
                .into_response();
        }
    };
    if let Err(problems) = prompt_explore::frontier::validate_grades_patch(&patch) {
        return (StatusCode::BAD_REQUEST, Json(problems)).into_response();
    }
    let mut jobs = state.jobs.lock().unwrap();
    let Some(job) = jobs.get_mut(&id) else {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "error": format!("no investigation '{id}' in this server's memory — ids come from POST /api/investigations and are lost on restart")
            })),
        )
            .into_response();
    };
    for (axis, value) in patch.grades {
        match value {
            Some(v) => {
                job.grades.insert(axis, v);
            }
            None => {
                job.grades.remove(&axis);
            }
        }
    }
    (
        StatusCode::OK,
        Json(GradesView {
            grades: job.grades.clone(),
        }),
    )
        .into_response()
}

/// Delete an investigation: remove the job — its traces, grades, and
/// progress — from the server's memory. Irreversible: the evidence is
/// gone (a re-run means POSTing a new investigation), and grades are
/// only stored on the job — read the job first if you want to keep
/// them. Useful for pruning a campaign's dead variants so the
/// dashboard and POST /api/frontier only show the points you still
/// compare. RUNNING jobs cannot be deleted (409): a run cannot be
/// cancelled — its provider calls would keep spending while the
/// result is discarded. Poll until done or failed, then delete.
#[utoipa::path(
    delete,
    path = "/api/investigations/{id}",
    params(("id" = String, Path, description = "Job id returned by POST /api/investigations")),
    security(("api_token" = [])),
    responses(
        (status = 200, description = "Deleted. Body: {\"deleted\": \"<id>\"}"),
        (status = 401, description = "Missing or invalid bearer token"),
        (status = 404, description = "Unknown job id (already deleted, or lost on restart)"),
        (status = 409, description = "Job is still running — wait for done/failed, then delete")
    )
)]
async fn delete_investigation(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    let mut jobs = state.jobs.lock().unwrap();
    match jobs.get(&id).map(|j| j.status) {
        None => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "error": format!("no investigation '{id}' in this server's memory — already deleted, or lost on restart (the job store is in-memory by design)")
            })),
        )
            .into_response(),
        Some(JobStatus::Running) => (
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "error": format!(
                    "investigation '{id}' is still running and cannot be deleted — a run \
                     cannot be cancelled: its provider calls would keep spending while the \
                     result is discarded. Poll GET /api/investigations/{id} until status is \
                     done or failed, then DELETE again"
                )
            })),
        )
            .into_response(),
        Some(_) => {
            jobs.remove(&id);
            (
                StatusCode::OK,
                Json(serde_json::json!({ "deleted": id })),
            )
                .into_response()
        }
    }
}

/// Assemble the harness-side facts the frontier needs from one job.
/// Thin: pure data plumbing, all logic lives in core::frontier.
fn snapshot_of(id: &str, job: &Job) -> InvestigationSnapshot {
    let result = job.result.as_ref();
    InvestigationSnapshot {
        id: id.to_string(),
        status: match job.status {
            JobStatus::Running => SnapshotStatus::Running,
            JobStatus::Done => SnapshotStatus::Done,
            JobStatus::Failed => SnapshotStatus::Failed,
        },
        put_id: Some(job.put.id.clone()).filter(|p| !p.is_empty()),
        grades: job.grades.clone(),
        usage: result.map(|r| r.usage),
        put_model: Some(job.model.clone()),
        sim_model: Some(job.sim_model.clone()),
        // A "step" is one tool call OR the final completion — the same
        // unit max_steps_per_trace budgets. Completed attempts only.
        steps_per_trace: result
            .map(|r| r.attempts.iter().map(|a| a.steps.len() as u64).collect())
            .unwrap_or_default(),
    }
}

#[derive(Deserialize, utoipa::ToSchema)]
struct FrontierQuery {
    /// `json` (default): points with on_frontier/dominated_by for
    /// programmatic optimization. `svg`: a 2-axis scatter plot with the
    /// frontier staircase (up-and-right is always better, whichever
    /// directions the axes have).
    #[serde(default)]
    format: Option<String>,
}

/// Compute the Pareto frontier over a caller-chosen set of
/// investigations and axes. This is the read side of multi-dimensional
/// prompt optimization: you run prompt variants (each an investigation),
/// grade the soft axes you care about (PATCH /api/investigations/{id}),
/// and then ask which variants are NOT dominated — on any mix of your
/// graded axes (tone_of_voice, …) and the harness's measured ones
/// (put_/sim_ tokens & cost, steps_per_trace statistics).
///
/// Dominance is N-dimensional; `format=svg` renders exactly 2 axes (a
/// v0 rendering constraint — send `format=json` for N axes). Axis
/// direction is declared HERE, per request (`"better": "lower" |
/// "higher"`), never stored. The harness records your judgment and does
/// arithmetic; it never interprets a grade — including which graded
/// values are "good enough" or what an axis should measure. Those are
/// caller-domain questions, answered when you PATCH grades from the
/// traces.
///
/// Every fixable problem (missing grade, unpriced cost axis, running
/// job, unknown id, duplicate id, bad label/color, direction conflict
/// with a reserved axis, …) comes back in ONE 422 body with typed
/// reasons, each detail naming the fix — including the exact PATCH to
/// make for a missing grade.
#[utoipa::path(
    post,
    path = "/api/frontier",
    params(("format" = Option<String>, Query, description = "`json` (default) or `svg`")),
    request_body(content = FrontierRequest, description = "Investigation ids (bare strings or {id, label?, color?} — labels `^[A-Za-z0-9_-]{1,64}$`, colors `#rrggbb`) plus axes (name + better). Ids must be unique; exactly 2 axes for format=svg."),
    security(("api_token" = [])),
    responses(
        (status = 200, description = "Frontier points. `format=json` (default): body = FrontierResponse (points with values, on_frontier, dominated_by — uuids, not labels, are the stable key). `format=svg`: body is `image/svg+xml`, a scatter plot with the non-dominated staircase; lower-is-better axes are pixel-inverted so up-and-right is always better.", body = FrontierResponse),
        (status = 400, description = "Malformed body or unknown ?format"),
        (status = 401, description = "Missing or invalid bearer token"),
        (status = 422, description = "Fixable problems, all collected: every detail names the fix (for a missing grade, the exact PATCH to make). Reasons: unknown_investigation, duplicate_investigation, job_running, job_failed, no_grade, axis_absent, direction_conflict, bad_axis_name, duplicate_axis, axis_arity, bad_label, bad_color, empty_investigations, empty_axes", body = FrontierError)
    )
)]
async fn frontier(
    State(state): State<Arc<AppState>>,
    Query(q): Query<FrontierQuery>,
    body: Result<Json<FrontierRequest>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let format = match q.format.as_deref() {
        None | Some("json") => FrontierFormat::Json,
        Some("svg") => FrontierFormat::Svg,
        Some(other) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": format!("unknown format '{other}' — use ?format=json or ?format=svg")
                })),
            )
                .into_response();
        }
    };
    let req = match body {
        Ok(Json(r)) => r,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": format!("body is not valid frontier request JSON: {e}")
                })),
            )
                .into_response();
        }
    };

    // Snapshot every referenced job under one lock, then compute
    // outside the lock (compute is pure and can be slow-ish for big
    // point sets; never hold the store lock through it).
    let snapshots: BTreeMap<String, InvestigationSnapshot> = {
        let jobs = state.jobs.lock().unwrap();
        req.investigations
            .iter()
            .map(|inv| inv.id().to_string())
            .collect::<Vec<_>>()
            .into_iter()
            .filter_map(|id| jobs.get(&id).map(|j| (id.clone(), snapshot_of(&id, j))))
            .collect()
    };

    match frontier::compute(&req, &snapshots, format) {
        Err(problems) => (StatusCode::UNPROCESSABLE_ENTITY, Json(problems)).into_response(),
        Ok(FrontierResponse { points }) => match format {
            FrontierFormat::Json => {
                (StatusCode::OK, Json(FrontierResponse { points })).into_response()
            }
            FrontierFormat::Svg => {
                // Unwrap safety: compute already validated exactly-2
                // axes for the svg format.
                let (x, y) = (&req.axes[0], &req.axes[1]);
                let svg = frontier::svg::render(
                    &points,
                    &frontier::svg::PlotAxis::new(&x.name, x.better),
                    &frontier::svg::PlotAxis::new(&y.name, y.better),
                );
                (
                    StatusCode::OK,
                    [(header::CONTENT_TYPE, "image/svg+xml")],
                    svg,
                )
                    .into_response()
            }
        },
    }
}

const INDEX_HTML: &str = include_str!("../static/index.html");
const VENDOR_PREACT: &str = include_str!("../static/vendor/preact.mjs");
const VENDOR_HOOKS: &str = include_str!("../static/vendor/hooks.mjs");
const VENDOR_HTM: &str = include_str!("../static/vendor/htm.mjs");

async fn vendor_preact() -> impl axum::response::IntoResponse {
    (
        [("content-type", "text/javascript;charset=utf-8")],
        VENDOR_PREACT,
    )
}
async fn vendor_hooks() -> impl axum::response::IntoResponse {
    (
        [("content-type", "text/javascript;charset=utf-8")],
        VENDOR_HOOKS,
    )
}
async fn vendor_htm() -> impl axum::response::IntoResponse {
    (
        [("content-type", "text/javascript;charset=utf-8")],
        VENDOR_HTM,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::INDEX_HTML;
    use axum::http::Request as HttpRequest;
    use tower::ServiceExt; // oneshot against the REAL router

    /// A state with no LLM client: enough for the grading/frontier
    /// surface (which is LLM-independent by design).
    fn test_state() -> Arc<AppState> {
        Arc::new(AppState {
            client: None,
            jobs: Mutex::new(HashMap::new()),
            default_provider: "zai".into(),
            models_client: prompt_explore::llm::GenaiClient::builder().build(),
            models_cache: Mutex::new(None),
            api_token: None,
        })
    }

    fn put(id: &str) -> PromptUnderTest {
        PromptUnderTest {
            id: id.into(),
            template: "You cancel orders.".into(),
            tools: vec![],
            design_goals: "Never cancel without an explicit user request.".into(),
        }
    }

    /// Seed a DONE job whose attempts have `steps_len` steps each and
    /// whose PUT model burned `out` output tokens.
    fn seed_done_job(
        state: &Arc<AppState>,
        id: &str,
        put_id: &str,
        out: u64,
        steps_len: Vec<usize>,
    ) {
        let (id, job) = fabricate_done_job(id, put_id, "demo template", out, &steps_len);
        state.jobs.lock().unwrap().insert(id, job);
    }

    async fn patch_grades(app: &Router, id: &str, body: &str) -> (StatusCode, serde_json::Value) {
        let res = app
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .method("PATCH")
                    .uri(format!("/api/investigations/{id}"))
                    .header("content-type", "application/json")
                    .body(body.to_string())
                    .unwrap(),
            )
            .await
            .unwrap();
        let code = res.status();
        let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
            .await
            .unwrap();
        (
            code,
            serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
        )
    }

    async fn post_frontier(app: &Router, query: &str, body: &str) -> (StatusCode, String, String) {
        let res = app
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .method("POST")
                    .uri(format!("/api/frontier{query}"))
                    .header("content-type", "application/json")
                    .body(body.to_string())
                    .unwrap(),
            )
            .await
            .unwrap();
        let code = res.status();
        let ct = res
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let body = String::from_utf8(
            axum::body::to_bytes(res.into_body(), 1 << 20)
                .await
                .unwrap()
                .to_vec(),
        )
        .unwrap();
        (code, ct, body)
    }

    #[tokio::test]
    async fn patch_grades_merges_deletes_and_echoes() {
        let state = test_state();
        seed_done_job(&state, "job-1", "cancel-bot", 100, vec![2]);
        let app = build_app(state);

        // Set two axes.
        let (code, v) = patch_grades(
            &app,
            "job-1",
            r#"{"grades": {"tone_of_voice": 0.8, "clarity": 0.6}}"#,
        )
        .await;
        assert_eq!(code, StatusCode::OK);
        assert_eq!(v["grades"]["tone_of_voice"], 0.8);
        assert_eq!(v["grades"]["clarity"], 0.6);

        // Overwrite one, delete the other; echo shows the merged map.
        let (code, v) = patch_grades(
            &app,
            "job-1",
            r#"{"grades": {"clarity": 0.9, "tone_of_voice": null}}"#,
        )
        .await;
        assert_eq!(code, StatusCode::OK);
        assert_eq!(v["grades"], serde_json::json!({"clarity": 0.9}));

        // Grades are visible on the job view (the UI reads this).
        let res = app
            .clone()
            .oneshot(
                HttpRequest::get("/api/investigations/job-1")
                    .body(String::new())
                    .unwrap(),
            )
            .await
            .unwrap();
        let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["grades"]["clarity"], 0.9);
    }

    #[tokio::test]
    async fn patch_grades_rejects_reserved_and_bad_names() {
        let state = test_state();
        seed_done_job(&state, "job-1", "cancel-bot", 100, vec![2]);
        let app = build_app(state);

        let (code, v) = patch_grades(&app, "job-1", r#"{"grades": {"put_cost_usd": 1.0}}"#).await;
        assert_eq!(code, StatusCode::BAD_REQUEST);
        assert_eq!(v["problems"][0]["reason"], "reserved_axis_name");

        let (code, v) = patch_grades(&app, "job-1", r#"{"grades": {"Bad Name": 1.0}}"#).await;
        assert_eq!(code, StatusCode::BAD_REQUEST);
        assert_eq!(v["problems"][0]["reason"], "bad_axis_name");

        let (code, v) = patch_grades(&app, "no-such-job", r#"{"grades": {"x": 1.0}}"#).await;
        assert_eq!(code, StatusCode::NOT_FOUND);
        assert!(v["error"].as_str().unwrap().contains("lost on restart"));
    }

    #[tokio::test]
    async fn frontier_json_and_svg_round_trip() {
        let state = test_state();
        seed_done_job(&state, "v1", "cancel-bot", 1450, vec![2, 4]);
        seed_done_job(&state, "v2", "cancel-bot", 2300, vec![2, 4]);
        seed_done_job(&state, "v3", "cancel-bot", 3100, vec![3, 3]); // dominated by v2
        let app = build_app(state.clone());
        patch_grades(&app, "v1", r#"{"grades": {"tone_of_voice": 0.4}}"#).await;
        patch_grades(&app, "v2", r#"{"grades": {"tone_of_voice": 0.85}}"#).await;
        patch_grades(&app, "v3", r#"{"grades": {"tone_of_voice": 0.75}}"#).await;

        let req_body = r#"{
            "investigations": ["v1", "v2", {"id": "v3", "label": "v3-verbose"}],
            "axes": [
                {"name": "put_output_tokens", "better": "lower"},
                {"name": "tone_of_voice", "better": "higher"}
            ]
        }"#;
        let (code, ct, body) = post_frontier(&app, "?format=json", req_body).await;
        assert_eq!(code, StatusCode::OK);
        assert!(ct.starts_with("application/json"));
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        let points: Vec<_> = v["points"].as_array().unwrap().clone();
        let find = |label: &str| -> serde_json::Value {
            points
                .iter()
                .find(|p| p["label"] == label)
                .unwrap_or_else(|| panic!("no point labeled {label} in {points:?}"))
                .clone()
        };
        // Mixed directions: v1 cheapest tokens (frontier despite low
        // tone), v2 best tone (frontier), v3 dominated by v2 on both.
        assert_eq!(find("cancel-bot")["on_frontier"], true);
        assert_eq!(find("cancel-bot#2")["on_frontier"], true);
        assert_eq!(find("v3-verbose")["on_frontier"], false);
        assert_eq!(
            find("v3-verbose")["dominated_by"],
            serde_json::json!(["v2"])
        );
        // Only the REQUESTED axes appear in values.
        assert!(
            find("cancel-bot")["values"]
                .get("steps_per_trace_avg")
                .is_none()
        );

        let (code, ct, body) = post_frontier(&app, "?format=svg", req_body).await;
        assert_eq!(code, StatusCode::OK);
        assert!(ct.starts_with("image/svg+xml"), "ct={ct}");
        assert!(body.starts_with("<svg "));
        assert!(body.contains("v3-verbose"));
    }

    #[tokio::test]
    async fn frontier_typed_problems_over_http() {
        let state = test_state();
        seed_done_job(&state, "v1", "cancel-bot", 1450, vec![2]);
        state.jobs.lock().unwrap().insert(
            "still-running".into(),
            Job {
                status: JobStatus::Running,
                result: None,
                error: None,
                progress: Arc::new(Mutex::new(RunProgress::default())),
                started_at: 0,
                reason: None,
                put: put("cancel-bot"),
                grades: BTreeMap::new(),
                scenarios: vec![],
                model: "zai_coding::glm-5.2".into(),
                sim_model: "zai_coding::glm-5.2".into(),
                workspace_files: 0,
            },
        );
        let app = build_app(state);

        let body = r#"{
            "investigations": ["still-running", "ghost", "v1", "v1"],
            "axes": [
                {"name": "put_cost_usd", "better": "lower"},
                {"name": "tone_of_voice", "better": "higher"}
            ]
        }"#;
        let (code, _ct, body) = post_frontier(&app, "", body).await;
        assert_eq!(code, StatusCode::UNPROCESSABLE_ENTITY);
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["error"], "frontier_request_invalid");
        let reasons: Vec<&str> = v["problems"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["reason"].as_str().unwrap())
            .collect();
        // put_cost_usd is unpriced on v1 (axis_absent) and v1 has no
        // tone grade (no_grade); the duplicated id reports ONE problem
        // (duplicate), not its axis problems twice.
        let mut sorted = reasons.clone();
        sorted.sort_unstable();
        assert_eq!(
            sorted,
            vec![
                "axis_absent",
                "duplicate_investigation",
                "job_running",
                "no_grade",
                "unknown_investigation"
            ]
        );
        // The no_grade detail names the exact PATCH (fix-instruction
        // contract, verifiable over HTTP).
        let no_grade = v["problems"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["reason"] == "no_grade")
            .unwrap()
            .clone();
        assert!(
            no_grade["detail"]
                .as_str()
                .unwrap()
                .contains("PATCH /api/investigations/v1")
        );
    }

    #[tokio::test]
    async fn frontier_rejects_svg_with_non_two_axes() {
        let state = test_state();
        seed_done_job(&state, "v1", "cancel-bot", 1450, vec![2]);
        let app = build_app(state);
        let (code, _ct, body) = post_frontier(
            &app,
            "?format=svg",
            r#"{"investigations": ["v1"], "axes": [{"name": "put_output_tokens", "better": "lower"}]}"#, // 1 axis
        )
        .await;
        assert_eq!(code, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(body.contains("axis_arity"));
    }

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
