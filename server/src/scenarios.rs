//! Scenario and probe endpoints.
//!
//! A scenario is the reusable half of an investigation: the world narrative,
//! the tool surface with optional caller-authored Lua implementations, the
//! simulator settings it runs under, and the initial workspace — uploaded once
//! and then reused, pinned by reference from each investigation.
//!
//! Simulation probes (`POST /api/scenarios/{id}/simulations`) execute
//! caller-submitted tool calls through the SAME engine an investigation uses,
//! so a caller can develop and test a world over HTTP with no local Lua
//! toolchain and without spending an investigation.
use super::*;
use prompt_explore::model::scenario::{Correction, ScenarioDefinition};
use prompt_explore::scenario::{
    ProbeProgress, ProbeRequest, ProbeStatus, ProbeStopReason, ProbeTarget, StoreError,
    WorkspaceAction, run_probe,
};
use prompt_explore::simulate::{Workspace, unpack_zip_with_limits};

/// One stored scenario, as the API reports it: the definition, its identity,
/// and the dependency bookkeeping a caller needs before editing or deleting it.
#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct ScenarioView {
    pub id: String,
    /// Increases on every edit. An investigation pins exactly one value, and
    /// `PATCH` requires the caller's expected revision.
    pub revision: u64,
    /// SHA-256 of the execution-relevant contents (narrative, domains,
    /// contracts, Lua sources, simulator settings). Display-only metadata such
    /// as `label` is excluded.
    pub definition_hash: String,
    /// SHA-256 of the initial workspace inventory (paths and bytes).
    pub workspace_hash: String,
    pub workspace_files: usize,
    pub label: Option<String>,
    /// Set when this scenario was forked as a correction of another one. Purely
    /// descriptive history; unrelated to dependency tracking.
    pub correction: Option<Correction>,
    pub created_at: u64,
    pub updated_at: u64,
    /// True while no investigation references this scenario. A referenced
    /// scenario is immutable: fork it to change it.
    pub editable: bool,
    /// Investigations pinning this scenario. They block edits and, without
    /// `cascade=true`, deletion.
    pub investigation_ids: Vec<String>,
    /// How many of the pinned investigations are still running.
    pub running_investigations: usize,
    /// Ids of probes recorded against this scenario (probes never pin it).
    pub probe_ids: Vec<String>,
    pub definition: ScenarioDefinition,
}

impl ScenarioView {
    fn build(
        record: &prompt_explore::scenario::ScenarioRecord,
        running: usize,
        probes: Vec<String>,
    ) -> Self {
        Self {
            id: record.id.clone(),
            revision: record.revision,
            definition_hash: record.definition_hash.clone(),
            workspace_hash: record.workspace.content_hash(),
            workspace_files: record.workspace.file_count(),
            label: record.label.clone(),
            correction: record.correction.clone(),
            created_at: record.created_at,
            updated_at: record.updated_at,
            editable: record.editable(),
            investigation_ids: record.investigation_ids(),
            running_investigations: running,
            probe_ids: probes,
            definition: record.definition.clone(),
        }
    }
}

/// A scenario summary without the definition body, for listing.
#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct ScenarioSummary {
    pub id: String,
    pub revision: u64,
    pub definition_hash: String,
    pub label: Option<String>,
    pub tools: usize,
    pub implemented_tools: usize,
    pub workspace_files: usize,
    pub editable: bool,
    pub investigations: usize,
    pub probes: usize,
    pub updated_at: u64,
}

/// Body of `POST /api/scenarios` (also the `request` part of the multipart
/// form, whose optional `workspace` part carries the initial .zip).
#[derive(Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct ScenarioCreateRequest {
    pub scenario: ScenarioDefinition,
    /// Optional display label. Never part of the identity or the hash.
    #[serde(default)]
    pub label: Option<String>,
}

/// Body of `PATCH /api/scenarios/{id}`.
///
/// Supplied collections REPLACE their previous values: there is no partial
/// deep merge of a definition. `expected_revision` is required, so a stale edit
/// can never silently overwrite another agent's work.
#[derive(Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct ScenarioPatchRequest {
    /// The revision the caller last read. A mismatch is a 409.
    pub expected_revision: u64,
    /// The complete new definition (replacing the stored one).
    pub scenario: ScenarioDefinition,
    /// Set a new label; `null` clears it. Omit to keep the current label.
    #[serde(default, deserialize_with = "deserialize_present_option")]
    pub label: Option<Option<String>>,
    /// Explicitly replace the initial workspace with an empty one.
    #[serde(default)]
    pub clear_workspace: bool,
}

/// Body of `POST /api/scenarios/{id}/fork`.
#[derive(Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct ScenarioForkRequest {
    /// Refuse to fork from a scenario that has moved on since the caller read it.
    #[serde(default)]
    pub expected_revision: Option<u64>,
    /// Optional description of what this copy corrects. Purely descriptive: it
    /// neither locks nor invalidates the predecessor, and survives its deletion.
    #[serde(default)]
    pub correction: Option<CorrectionRequest>,
    #[serde(default)]
    pub label: Option<String>,
}

#[derive(Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct CorrectionRequest {
    /// Explanation of what changed and why.
    pub reason: String,
    /// The predecessor revision this copy was made from; defaults to the
    /// current revision.
    #[serde(default)]
    pub revision: Option<u64>,
}

/// One simulation probe: caller-submitted tool calls rendered through the
/// investigation engine, with complete per-call provenance.
#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct ProbeView {
    pub id: String,
    pub scenario_id: String,
    /// The scenario revision this probe actually ran against (a probe does not
    /// pin the scenario, so record what it used).
    pub scenario_revision: u64,
    pub scenario_hash: String,
    pub status: ProbeStatus,
    pub stop_reason: Option<ProbeStopReason>,
    pub started_at: u64,
    pub finished_at: Option<u64>,
    /// The inputs this probe ran with: sampled, or the caller's bindings.
    pub resolved_inputs: HashMap<String, Value>,
    pub calls: Vec<prompt_explore::scenario::ProbeCall>,
    pub error: Option<String>,
    /// Free-form note supplied with the probe, echoed for context.
    pub reason: Option<String>,
    /// Simulator usage for this probe, with cost when the model catalog prices it.
    pub usage: Option<UsageTotals>,
    pub cost_usd: Option<f64>,
}

/// The stored state of one probe: identity, the run's live progress, and usage.
pub(super) struct ProbeRecord {
    pub id: String,
    pub scenario_id: String,
    pub scenario_revision: u64,
    pub scenario_hash: String,
    pub reason: Option<String>,
    pub progress: Arc<Mutex<ProbeProgress>>,
    /// The usage tracker for a provider-backed probe. Kept so a reader always
    /// sees live totals, even when it polls the instant the probe stops (the
    /// probe's own task may not have stored its final figure yet).
    pub tracker: Option<Arc<UsageTracker>>,
    /// The resolved simulator model this probe runs on, for cost attribution.
    pub sim_model: String,
    pub usage: UsageTotals,
    pub cost_usd: Option<f64>,
}

impl ProbeRecord {
    /// Refresh usage/cost from the live tracker, if any, with a pricing map the
    /// caller has already fetched. Called by the read handlers so a poll cannot
    /// race the probe's own finalization.
    pub fn refresh_usage(&mut self, pricing: &prompt_explore::llm::PricingMap) {
        let Some(tracker) = &self.tracker else {
            return;
        };
        let mut usage = tracker.totals();
        usage.cost_usd = pricing.get(&self.sim_model).and_then(|price| {
            cost_usd(
                usage.input_tokens,
                usage.cache_read_tokens,
                usage.output_tokens,
                price,
            )
        });
        self.usage = usage.clone();
        self.cost_usd = usage.cost_usd;
    }

    pub fn view(&self) -> ProbeView {
        let progress = self.progress.lock().unwrap().clone();
        ProbeView {
            id: self.id.clone(),
            scenario_id: self.scenario_id.clone(),
            scenario_revision: self.scenario_revision,
            scenario_hash: self.scenario_hash.clone(),
            status: progress.status,
            stop_reason: progress.stop_reason,
            started_at: progress.started_at,
            finished_at: progress.finished_at,
            resolved_inputs: progress.resolved_inputs,
            calls: progress.calls,
            error: progress.error,
            reason: self.reason.clone(),
            usage: (self.usage.llm_calls > 0).then(|| self.usage.clone()),
            cost_usd: self.cost_usd,
        }
    }
}

/// HTTP status for a registry conflict or validation failure.
pub(super) fn store_error_response(error: StoreError) -> Response {
    let status = match error {
        StoreError::NotFound(_) => StatusCode::NOT_FOUND,
        StoreError::Locked { .. }
        | StoreError::StaleRevision { .. }
        | StoreError::HasDependents { .. }
        | StoreError::Running { .. } => StatusCode::CONFLICT,
        StoreError::Invalid(_) => StatusCode::BAD_REQUEST,
    };
    (
        status,
        Json(serde_json::json!({ "error": error.to_string() })),
    )
        .into_response()
}

fn bad_request(message: impl Into<String>) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(serde_json::json!({ "error": message.into() })),
    )
        .into_response()
}

/// Parse a multipart scenario body: a required `request` part and an optional
/// `workspace` part (a .zip seeding the initial workspace).
async fn parse_scenario_multipart<T: serde::de::DeserializeOwned>(
    req: Request,
    state: &Arc<AppState>,
) -> Result<(T, Option<Workspace>), String> {
    let mut multipart = Multipart::from_request(req, state)
        .await
        .map_err(|e| format!("could not begin multipart parsing: {e}"))?;
    let mut request: Option<T> = None;
    let mut workspace: Option<Workspace> = None;
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
                request = Some(serde_json::from_slice(&bytes).map_err(|e| {
                    format!("the 'request' part is not valid JSON for this endpoint: {e}")
                })?);
            }
            "workspace" => {
                let bytes = field
                    .bytes()
                    .await
                    .map_err(|e| format!("could not read 'workspace' part: {e}"))?;
                workspace = Some(
                    unpack_zip_with_limits(
                        &bytes,
                        workspace_compressed_limit(),
                        workspace_decompressed_limit(),
                    )
                    .map_err(|e| e.to_string())?,
                );
            }
            other => {
                return Err(format!(
                    "unknown multipart part '{other}'; expected 'request' and optional 'workspace'"
                ));
            }
        }
    }
    let request = request
        .ok_or_else(|| "multipart body is missing the required 'request' part".to_string())?;
    Ok((request, workspace))
}

/// Read a JSON body of type `T`, or a multipart body with a `request` part.
async fn read_request<T: serde::de::DeserializeOwned>(
    req: Request,
    state: &Arc<AppState>,
) -> Result<(T, Option<Workspace>), Response> {
    let content_type = req
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();
    if content_type.starts_with("multipart/") {
        parse_scenario_multipart::<T>(req, state)
            .await
            .map_err(bad_request)
    } else {
        let bytes = match to_bytes(req.into_body(), investigation_body_limit()).await {
            Ok(bytes) => bytes,
            Err(error) => {
                return Err(bad_request(format!("could not read request body: {error}")));
            }
        };
        serde_json::from_slice(&bytes)
            .map(|value| (value, None))
            .map_err(|error| bad_request(format!("body is not valid JSON: {error}")))
    }
}

fn probe_ids(state: &AppState, scenario_id: &str) -> Vec<String> {
    state
        .probes
        .lock()
        .unwrap()
        .values()
        .filter(|probe| probe.scenario_id == scenario_id)
        .map(|probe| probe.id.clone())
        .collect()
}

fn running_investigations(
    state: &AppState,
    record: &prompt_explore::scenario::ScenarioRecord,
) -> usize {
    let jobs = state.jobs.lock().unwrap();
    record
        .investigation_ids()
        .iter()
        .filter(|id| {
            jobs.get(*id)
                .is_some_and(|job| matches!(job.status, JobStatus::Running))
        })
        .count()
}

/// Register a scenario: the world, its tools (with optional Lua
/// implementations), the simulator settings, and an optional initial workspace
/// archive. Creating a scenario executes nothing and calls no provider.
#[utoipa::path(
    post,
    path = "/api/scenarios",
    request_body(content = inline(ScenarioCreateRequest), content_type = "application/json"),
    security(("api_token" = [])),
    responses(
        (status = 201, description = "Registered. Body: {id, revision, definition_hash, workspace_hash}", body = JobCreated),
        (status = 400, description = "Body is not valid JSON, or the definition fails validation (duplicate tool names, unparsable Lua, limits out of range)"),
        (status = 401, description = "Missing or invalid bearer token")
    )
)]
pub(super) async fn create_scenario(State(state): State<Arc<AppState>>, req: Request) -> Response {
    let (request, workspace) = match read_request::<ScenarioCreateRequest>(req, &state).await {
        Ok(parsed) => parsed,
        Err(response) => return response,
    };
    let mut store = state.scenarios.lock().unwrap();
    match store.create(
        request.scenario,
        workspace.unwrap_or_else(Workspace::empty),
        request.label,
        epoch_millis(),
    ) {
        Ok(id) => {
            let record = store.get(&id).expect("just created");
            let body = serde_json::json!({
                "id": id,
                "revision": record.revision,
                "definition_hash": record.definition_hash,
                "workspace_hash": record.workspace.content_hash(),
            });
            (StatusCode::CREATED, Json(body)).into_response()
        }
        Err(error) => store_error_response(error),
    }
}

/// List stored scenarios (summaries; no definition bodies).
#[utoipa::path(
    get,
    path = "/api/scenarios",
    security(("api_token" = [])),
    responses(
        (status = 200, description = "All stored scenarios in this server's memory", body = Vec<ScenarioSummary>),
        (status = 401, description = "Missing or invalid bearer token")
    )
)]
pub(super) async fn list_scenarios(
    State(state): State<Arc<AppState>>,
) -> Json<Vec<ScenarioSummary>> {
    let store = state.scenarios.lock().unwrap();
    let mut summaries: Vec<ScenarioSummary> = store
        .list()
        .iter()
        .map(|record| ScenarioSummary {
            id: record.id.clone(),
            revision: record.revision,
            definition_hash: record.definition_hash.clone(),
            label: record.label.clone(),
            tools: record.definition.tools.len(),
            implemented_tools: record.definition.implementations().len(),
            workspace_files: record.workspace.file_count(),
            editable: record.editable(),
            investigations: record.investigation_count(),
            probes: probe_ids(&state, &record.id).len(),
            updated_at: record.updated_at,
        })
        .collect();
    summaries.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    Json(summaries)
}

/// Read one scenario: its definition, identity, and dependents.
#[utoipa::path(
    get,
    path = "/api/scenarios/{id}",
    params(("id" = String, Path, description = "Scenario id")),
    security(("api_token" = [])),
    responses(
        (status = 200, description = "The stored scenario", body = ScenarioView),
        (status = 401, description = "Missing or invalid bearer token"),
        (status = 404, description = "Unknown scenario (already deleted, or lost on restart)")
    )
)]
pub(super) async fn get_scenario(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    let store = state.scenarios.lock().unwrap();
    let Some(record) = store.get(&id) else {
        return store_error_response(StoreError::NotFound(id));
    };
    let running = running_investigations(&state, record);
    let view = ScenarioView::build(record, running, probe_ids(&state, &id));
    Json(view).into_response()
}

/// Edit a scenario's definition, simulator settings, label, or initial
/// workspace. Refused while any investigation references the scenario (fork it
/// instead) and while `expected_revision` is stale.
#[utoipa::path(
    patch,
    path = "/api/scenarios/{id}",
    params(("id" = String, Path, description = "Scenario id")),
    request_body(content = inline(ScenarioPatchRequest), content_type = "application/json"),
    security(("api_token" = [])),
    responses(
        (status = 200, description = "Edited. Body: {id, revision, definition_hash, workspace_hash}", body = JobCreated),
        (status = 400, description = "Body is not valid JSON, or the definition fails validation"),
        (status = 401, description = "Missing or invalid bearer token"),
        (status = 404, description = "Unknown scenario"),
        (status = 409, description = "The scenario is pinned by investigations, or expected_revision is stale")
    )
)]
pub(super) async fn patch_scenario(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    req: Request,
) -> Response {
    let (request, workspace) = match read_request::<ScenarioPatchRequest>(req, &state).await {
        Ok(parsed) => parsed,
        Err(response) => return response,
    };
    let action = match (workspace, request.clear_workspace) {
        (Some(workspace), _) => WorkspaceAction::Replace(workspace),
        (None, true) => WorkspaceAction::Clear,
        (None, false) => WorkspaceAction::Keep,
    };
    let mut store = state.scenarios.lock().unwrap();
    match store.replace(
        &id,
        request.expected_revision,
        request.scenario,
        action,
        request.label,
        epoch_millis(),
    ) {
        Ok(revision) => {
            let record = store.get(&id).expect("just edited");
            let body = serde_json::json!({
                "id": id,
                "revision": revision,
                "definition_hash": record.definition_hash,
                "workspace_hash": record.workspace.content_hash(),
            });
            Json(body).into_response()
        }
        Err(error) => store_error_response(error),
    }
}

/// Fork a scenario into a new editable one, sharing the immutable initial
/// workspace (no re-upload, no re-decompression). Optionally record what the
/// copy corrects.
#[utoipa::path(
    post,
    path = "/api/scenarios/{id}/fork",
    params(("id" = String, Path, description = "Scenario to copy")),
    request_body(content = inline(ScenarioForkRequest), content_type = "application/json"),
    security(("api_token" = [])),
    responses(
        (status = 201, description = "Forked. Body: {id, revision, definition_hash, workspace_hash}", body = JobCreated),
        (status = 400, description = "Body is not valid JSON"),
        (status = 401, description = "Missing or invalid bearer token"),
        (status = 404, description = "Unknown scenario"),
        (status = 409, description = "expected_revision does not match the current revision")
    )
)]
pub(super) async fn fork_scenario(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    req: Request,
) -> Response {
    let (request, _) = match read_request::<ScenarioForkRequest>(req, &state).await {
        Ok(parsed) => parsed,
        Err(response) => return response,
    };
    let mut store = state.scenarios.lock().unwrap();
    let current = match store.get(&id) {
        Some(record) => record.revision,
        None => return store_error_response(StoreError::NotFound(id)),
    };
    if let Some(expected) = request.expected_revision {
        if expected != current {
            return store_error_response(StoreError::StaleRevision {
                id: id.clone(),
                expected,
                current,
            });
        }
    }
    let correction = request.correction.map(|correction| Correction {
        scenario_id: id.clone(),
        revision: correction.revision.unwrap_or(current),
        reason: correction.reason,
    });
    match store.fork(&id, correction, request.label, epoch_millis()) {
        Ok(forked) => {
            let record = store.get(&forked).expect("just forked");
            let body = serde_json::json!({
                "id": forked,
                "revision": record.revision,
                "definition_hash": record.definition_hash,
                "workspace_hash": record.workspace.content_hash(),
            });
            (StatusCode::CREATED, Json(body)).into_response()
        }
        Err(error) => store_error_response(error),
    }
}

/// Delete a scenario. Refused while any referencing investigation is running
/// (a run cannot be cancelled), and refused without `cascade=true` when
/// finished investigations still pin it. With `cascade=true` those
/// investigations — their traces, grades and assessments — are deleted too.
#[utoipa::path(
    delete,
    path = "/api/scenarios/{id}",
    params(
        ("id" = String, Path, description = "Scenario id"),
        ("cascade" = Option<bool>, Query, description = "Delete investigations that reference this scenario instead of refusing (they are gone, with their traces and grades)")
    ),
    security(("api_token" = [])),
    responses(
        (status = 200, description = "Deleted. Body: {deleted, cascade_investigations, cascade_probes}"),
        (status = 401, description = "Missing or invalid bearer token"),
        (status = 404, description = "Unknown scenario"),
        (status = 409, description = "Investigations depend on this scenario (and cascade was not requested), or referenced work is still running")
    )
)]
pub(super) async fn delete_scenario(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(query): Query<DeleteScenarioQuery>,
) -> Response {
    let cascade = query.cascade.unwrap_or(false);
    let is_running = |investigation: &str| {
        state
            .jobs
            .lock()
            .unwrap()
            .get(investigation)
            .is_some_and(|job| matches!(job.status, JobStatus::Running))
    };
    // A running probe has no job record to consult, so check it here: deleting
    // a scenario whose probe is mid-flight would discard live work.
    // (Probes are short-lived, so this is a courtesy, not a locking rule.)
    let mut store = state.scenarios.lock().unwrap();
    let outcome = match store.delete(&id, cascade, is_running) {
        Ok(outcome) => outcome,
        Err(error) => return store_error_response(error),
    };
    drop(store);
    let mut jobs = state.jobs.lock().unwrap();
    for investigation in &outcome.cascade_investigations {
        jobs.remove(investigation);
    }
    drop(jobs);
    let mut probes = state.probes.lock().unwrap();
    let removed_probes: Vec<String> = probes
        .keys()
        .filter(|probe| probes[*probe].scenario_id == id)
        .cloned()
        .collect();
    for probe in &removed_probes {
        probes.remove(probe);
    }
    Json(serde_json::json!({
        "deleted": outcome.deleted,
        "cascade_investigations": outcome.cascade_investigations,
        "cascade_probes": removed_probes,
    }))
    .into_response()
}

#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct DeleteScenarioQuery {
    #[serde(default)]
    pub cascade: Option<bool>,
}

/// Export the initial workspace as a JSON file inventory (paths and contents).
/// This is the bytes every run starts from; a `workspace_hash` alone is not a
/// reproducible workspace.
#[utoipa::path(
    get,
    path = "/api/scenarios/{id}/workspace",
    params(("id" = String, Path, description = "Scenario id")),
    security(("api_token" = [])),
    responses(
        (status = 200, description = "The initial workspace inventory: {workspace_hash, files:[{path, bytes, content (base64)}]}"),
        (status = 401, description = "Missing or invalid bearer token"),
        (status = 404, description = "Unknown scenario")
    )
)]
pub(super) async fn get_scenario_workspace(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    let store = state.scenarios.lock().unwrap();
    let Some(record) = store.get(&id) else {
        return store_error_response(StoreError::NotFound(id));
    };
    let files: Vec<Value> = record
        .workspace
        .inventory()
        .into_iter()
        .map(|(path, bytes)| {
            serde_json::json!({
                "path": path,
                "bytes": bytes.len(),
                "content_base64": Base64::encode(&bytes),
            })
        })
        .collect();
    Json(serde_json::json!({
        "scenario_id": id,
        "revision": record.revision,
        "workspace_hash": record.workspace.content_hash(),
        "files": files,
    }))
    .into_response()
}

/// Submit a simulation probe: an ordered list of tool calls rendered against
/// this scenario through the SAME engine an investigation uses. Calls run
/// sequentially in one session from a fresh snapshot; the prompt under test is
/// never involved, and the result is never a frontier candidate.
///
/// Poll `GET /api/scenarios/{id}/simulations/{probe_id}` for the result.
#[utoipa::path(
    post,
    path = "/api/scenarios/{id}/simulations",
    params(("id" = String, Path, description = "Scenario id")),
    request_body = ProbeRequest,
    security(("api_token" = [])),
    responses(
        (status = 202, description = "Accepted. Body: {id, scenario_revision, scenario_hash}"),
        (status = 400, description = "Body is not valid JSON or the request fails validation"),
        (status = 401, description = "Missing or invalid bearer token"),
        (status = 404, description = "Unknown scenario"),
        (status = 409, description = "expected_revision does not match the current revision")
    )
)]
pub(super) async fn create_probe(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    req: Request,
) -> Response {
    let (request, _) = match read_request::<ProbeRequest>(req, &state).await {
        Ok(parsed) => parsed,
        Err(response) => return response,
    };
    if let Err(error) = request.validate() {
        return bad_request(error);
    }
    // Snapshot the scenario now: an edit while the probe runs must not change
    // what the probe is testing.
    let (target, simulator_model, settings) = {
        let store = state.scenarios.lock().unwrap();
        let Some(record) = store.get(&id) else {
            return store_error_response(StoreError::NotFound(id));
        };
        if let Some(expected) = request.expected_revision {
            if expected != record.revision {
                return store_error_response(StoreError::StaleRevision {
                    id: id.clone(),
                    expected,
                    current: record.revision,
                });
            }
        }
        (
            ProbeTarget::from_record(record, record.workspace.clone()),
            record.definition.simulation.sim_model.clone(),
            record.definition.simulation.clone(),
        )
    };
    // A probe with no declared inputs and full Lua coverage needs no provider:
    // develop a simulation offline, and let any delegation fail loudly per call.
    // A provider client is wrapped so the probe reports usage and cost.
    let tracker = state
        .client
        .clone()
        .map(|client| Arc::new(UsageTracker::new(client)));
    let sim_model = prompt_explore::llm::qualify_model(
        simulator_model.as_deref().unwrap_or(MODEL),
        &state.default_provider,
    );
    let probe_id = Uuid::new_v4().to_string();
    let progress = Arc::new(Mutex::new(ProbeProgress::new(epoch_millis())));
    state.probes.lock().unwrap().insert(
        probe_id.clone(),
        ProbeRecord {
            id: probe_id.clone(),
            scenario_id: id.clone(),
            scenario_revision: target.revision,
            scenario_hash: target.definition_hash.clone(),
            reason: request.reason.clone(),
            progress: progress.clone(),
            tracker: tracker.clone(),
            sim_model: sim_model.clone(),
            usage: UsageTotals::default(),
            cost_usd: None,
        },
    );
    let body = serde_json::json!({
        "id": probe_id,
        "scenario_revision": target.revision,
        "scenario_hash": target.definition_hash,
    });

    let state2 = state.clone();
    let probe_id2 = probe_id.clone();
    let client: Arc<dyn prompt_explore::llm::LlmClient> = match tracker.clone() {
        Some(tracker) => tracker as Arc<dyn prompt_explore::llm::LlmClient>,
        None => Arc::new(prompt_explore::llm::UnavailableClient),
    };
    tokio::spawn(async move {
        run_probe(
            client,
            sim_model.clone(),
            settings.sim_thinking_level,
            &target,
            &request,
            progress.clone(),
            epoch_millis,
        )
        .await;
        if tracker.is_none() {
            return;
        }
        let pricing = catalog_pricing_map(&models_cached(&state2).await.providers);
        if let Some(probe) = state2.probes.lock().unwrap().get_mut(&probe_id2) {
            probe.refresh_usage(&pricing);
        }
    });

    (StatusCode::ACCEPTED, Json(body)).into_response()
}

/// List probes recorded against a scenario (most recent first).
#[utoipa::path(
    get,
    path = "/api/scenarios/{id}/simulations",
    params(("id" = String, Path, description = "Scenario id")),
    security(("api_token" = [])),
    responses(
        (status = 200, description = "Probe summaries for this scenario", body = Vec<ProbeView>),
        (status = 401, description = "Missing or invalid bearer token"),
        (status = 404, description = "Unknown scenario")
    )
)]
pub(super) async fn list_probes(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    if state.scenarios.lock().unwrap().get(&id).is_none() {
        return store_error_response(StoreError::NotFound(id));
    }
    let pricing = catalog_pricing_map(&models_cached(&state).await.providers);
    let mut probes = state.probes.lock().unwrap();
    for probe in probes.values_mut() {
        probe.refresh_usage(&pricing);
    }
    let mut views: Vec<ProbeView> = probes
        .values()
        .filter(|probe| probe.scenario_id == id)
        .map(ProbeRecord::view)
        .collect();
    views.sort_by(|a, b| b.started_at.cmp(&a.started_at));
    Json(views).into_response()
}

/// Read one probe: every call's request, response, and provenance.
#[utoipa::path(
    get,
    path = "/api/scenarios/{id}/simulations/{probe_id}",
    params(
        ("id" = String, Path, description = "Scenario id"),
        ("probe_id" = String, Path, description = "Probe id returned by POST")
    ),
    security(("api_token" = [])),
    responses(
        (status = 200, description = "The probe, live or terminal, with complete per-call evidence", body = ProbeView),
        (status = 401, description = "Missing or invalid bearer token"),
        (status = 404, description = "Unknown scenario or probe")
    )
)]
pub(super) async fn get_probe(
    State(state): State<Arc<AppState>>,
    Path((id, probe_id)): Path<(String, String)>,
) -> Response {
    let pricing = catalog_pricing_map(&models_cached(&state).await.providers);
    let mut probes = state.probes.lock().unwrap();
    match probes.get_mut(&probe_id) {
        Some(probe) if probe.scenario_id == id => {
            probe.refresh_usage(&pricing);
            Json(probe.view()).into_response()
        }
        _ => store_error_response(StoreError::NotFound(format!(
            "probe '{probe_id}' for scenario '{id}'"
        ))),
    }
}

/// Base64 without pulling in a dependency: a 12-line encoder over the standard
/// alphabet is less code than another crate, and this is the only use.
struct Base64;

impl Base64 {
    const ALPHABET: &'static [u8; 64] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    fn encode(bytes: &[u8]) -> String {
        let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
        for chunk in bytes.chunks(3) {
            let b = [
                chunk[0],
                *chunk.get(1).unwrap_or(&0),
                *chunk.get(2).unwrap_or(&0),
            ];
            let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
            out.push(Self::ALPHABET[(n >> 18) as usize & 63] as char);
            out.push(Self::ALPHABET[(n >> 12) as usize & 63] as char);
            out.push(if chunk.len() > 1 {
                Self::ALPHABET[(n >> 6) as usize & 63] as char
            } else {
                '='
            });
            out.push(if chunk.len() > 2 {
                Self::ALPHABET[n as usize & 63] as char
            } else {
                '='
            });
        }
        out
    }
}
