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
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{Html, Json},
    routing::{get, post},
    Router,
};
use serde::{Deserialize, Serialize};

use prompt_explore::generate::{Investigator, LlmRole};
use prompt_explore::judge::render_transcript;
use prompt_explore::llm::OpenAiCompatibleClient;
use prompt_explore::model::input::{Investigation, PromptsUnderTest};
use prompt_explore::model::output::RunResult;

const MODEL: &str = "glm-5.2";

#[derive(Default)]
struct AppState {
    client: Option<Arc<OpenAiCompatibleClient>>,
    jobs: Mutex<HashMap<String, Job>>,
    next_id: AtomicU64,
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

#[derive(Serialize, Clone)]
struct InvestigateResponse {
    result: RunResult,
    /// Pre-rendered transcript of the witness trace, for display.
    transcript: Option<String>,
    scenarios_generated: usize,
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
    transcript: String,
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
        ..Default::default()
    });

    let app = Router::new()
        .route("/", get(index))
        .route("/api/investigations", post(create_investigation))
        .route("/api/investigations/{id}", get(get_investigation))
        .with_state(state);

    let addr = std::env::var("PROMPT_EXPLORE_ADDR").unwrap_or_else(|_| "127.0.0.1:8080".into());
    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    eprintln!("prompt-explore server listening on http://{addr}");
    axum::serve(listener, app).await.unwrap();
}

async fn index() -> Html<&'static str> {
    Html(INDEX_HTML)
}

async fn create_investigation(
    State(state): State<Arc<AppState>>,
    Json(req): Json<InvestigateRequest>,
) -> (StatusCode, Json<JobCreated>) {
    let id = format!("job-{}", state.next_id.fetch_add(1, Ordering::Relaxed) + 1);
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

        let transcript = outcome.result.witness.as_ref().map(|w| {
            w.traces
                .iter()
                .map(render_transcript)
                .collect::<Vec<_>>()
                .join("\n")
        });

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
                transcript: render_transcript(&a.trace),
            })
            .collect();

        let mut jobs = state2.jobs.lock().unwrap();
        if let Some(job) = jobs.get_mut(&id2) {
            job.status = JobStatus::Done;
            job.result = Some(InvestigateResponse {
                result: outcome.result,
                transcript,
                scenarios_generated: outcome.scenarios.len(),
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

const INDEX_HTML: &str = include_str!("../static/index.html");
