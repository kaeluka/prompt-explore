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
    extract::{Path, State},
    http::StatusCode,
    response::{Html, IntoResponse, Json},
    routing::{get, post},
    Router,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use prompt_explore::generate::{Investigator, LlmRole, ProposalApplier};
use prompt_explore::llm::OpenAiCompatibleClient;
use prompt_explore::model::input::{Investigation, PromptsUnderTest};
use prompt_explore::model::output::{Proposal, RunResult};
use prompt_explore::model::simulation::TraceStep;

const MODEL: &str = "glm-5.2";

struct AppState {
    client: Option<Arc<OpenAiCompatibleClient>>,
    jobs: Mutex<HashMap<String, Job>>,
}

struct Job {
    status: JobStatus,
    result: Option<InvestigateResponse>,
    error: Option<String>,
}

#[derive(Clone, Copy, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
enum JobStatus {
    Running,
    Done,
    Failed,
}

#[derive(Deserialize)]
struct InvestigateRequest {
    investigation: Investigation,
    psut: PromptsUnderTest,
}

#[derive(Deserialize)]
struct ApplyRequest {
    psut: PromptsUnderTest,
    proposal: Proposal,
    /// Which PUT to apply the proposal to. Defaults to the first.
    target_put: Option<String>,
}

#[derive(Serialize)]
struct ApplyResponse {
    psut: PromptsUnderTest,
    template_diff: Vec<prompt_explore::generate::DiffPart>,
    goals_diff: Vec<prompt_explore::generate::DiffPart>,
}

#[derive(Serialize, Clone)]
struct InvestigateResponse {
    result: RunResult,
    scenarios_generated: usize,
    /// The opening user message of the witness scenario, so the UI can
    /// show the full conversation (the trace steps start with the
    /// agent's first reply).
    witness_user_message: Option<String>,
    /// Every completed run — the evidence behind a negative result.
    attempts: Vec<AttemptView>,
}

#[derive(Serialize, Clone)]
struct AttemptView {
    user_message: Option<String>,
    hypothesis_id: String,
    matched: bool,
    verdict_rationale: String,
    verdict_confidence: Option<f32>,
    /// Structured steps, rendered as HTML by the UI.
    steps: Vec<TraceStep>,
}

#[derive(Serialize)]
struct JobCreated {
    id: String,
}

#[derive(Serialize)]
struct JobView {
    status: JobStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<InvestigateResponse>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[tokio::main]
async fn main() {
    let key = std::env::var("ZAI_API_KEY").expect("set ZAI_API_KEY");
    let state = Arc::new(AppState {
        client: Some(Arc::new(OpenAiCompatibleClient::zai(&key))),
        jobs: Mutex::new(HashMap::new()),
    });

    let app = Router::new()
        .route("/", get(index))
        .route("/api/investigations", post(create_investigation))
        .route("/api/investigations/{id}", get(get_investigation))
        .route("/api/apply", post(apply_proposal))
        .with_state(state);

    let addr = std::env::var("PROMPT_EXPLORE_ADDR").unwrap_or_else(|_| "127.0.0.1:8080".into());
    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    eprintln!("prompt-explore server listening on http://{addr}");
    axum::serve(listener, app).await.unwrap();
}

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

async fn create_investigation(
    State(state): State<Arc<AppState>>,
    Json(req): Json<InvestigateRequest>,
) -> (StatusCode, Json<JobCreated>) {
    let id = Uuid::new_v4().to_string();
    state.jobs.lock().unwrap().insert(
        id.clone(),
        Job {
            status: JobStatus::Running,
            result: None,
            error: None,
        },
    );

    let state2 = state.clone();
    let id2 = id.clone();
    tokio::spawn(async move {
        let client = state2.client.as_ref().unwrap().clone();
        let role = || LlmRole {
            client: client.clone(),
            model: MODEL.into(),
        };
        let investigator = Investigator {
            hypothesizer: role(),
            builder: role(),
            runner_put: role(),
            runner_sim: role(),
            judge: role(),
            proposer: role(),
            scenarios_per_hypothesis: 2,
            max_hypotheses: 4,
        };

        let outcome = investigator
            .investigate(&req.investigation, &req.psut)
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
            })
            .collect();

        let mut jobs = state2.jobs.lock().unwrap();
        if let Some(job) = jobs.get_mut(&id2) {
            job.status = JobStatus::Done;
            job.result = Some(InvestigateResponse {
                result: outcome.result,
                scenarios_generated: outcome.scenarios.len(),
                witness_user_message,
                attempts,
            });
        }
    });

    (StatusCode::ACCEPTED, Json(JobCreated { id }))
}

async fn get_investigation(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<JobView>, StatusCode> {
    let jobs = state.jobs.lock().unwrap();
    let job = jobs.get(&id).ok_or(StatusCode::NOT_FOUND)?;
    Ok(Json(JobView {
        status: job.status,
        result: job.result.clone(),
        error: job.error.clone(),
    }))
}

async fn apply_proposal(
    State(state): State<Arc<AppState>>,
    Json(req): Json<ApplyRequest>,
) -> Result<Json<ApplyResponse>, (StatusCode, String)> {
    let mut psut = req.psut;
    let target = req.target_put.unwrap_or_else(|| {
        psut.prompts
            .first()
            .map(|p| p.id.clone())
            .unwrap_or_default()
    });
    let put = psut
        .prompts
        .iter()
        .find(|p| p.id == target)
        .cloned()
        .ok_or_else(|| {
            (
                StatusCode::BAD_REQUEST,
                format!("target PUT '{target}' not found"),
            )
        })?;

    let client = state.client.as_ref().unwrap().clone();
    let applier = ProposalApplier::new(client, MODEL);
    let applied = applier
        .apply(&put, &req.proposal)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let template_diff = applied.template_diff.clone();
    let goals_diff = applied.goals_diff.clone();
    if let Some(p) = psut.prompts.iter_mut().find(|p| p.id == target) {
        p.template = applied.template;
        p.design_goals = applied.design_goals;
    }
    Ok(Json(ApplyResponse {
        psut,
        template_diff,
        goals_diff,
    }))
}

const INDEX_HTML: &str = include_str!("../static/index.html");
