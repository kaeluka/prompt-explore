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

use prompt_explore::assessment::Assessment;
use prompt_explore::frontier::attributes::{
    self, system_attributes, validate_attribute_patch, validate_post_attributes,
};
use prompt_explore::frontier::grouped::{
    self, GroupedFrontierRequest, GroupedFrontierResponse, GroupedSnapshot,
};
use prompt_explore::frontier::{
    self, FrontierFormat, GradesPatch, InvestigationSnapshot, SnapshotStatus,
};
use prompt_explore::generate::{Investigator, LlmRole};
use prompt_explore::llm::{
    ProviderClient, ProviderModels, ThinkingLevel, UsageByRole, UsageTotals, UsageTracker,
    catalog_pricing_map, cost_usd, list_all_map,
};
use prompt_explore::model::input::{Budget, Investigation, PromptUnderTest};
use prompt_explore::model::lua::LuaOptions;
use prompt_explore::model::output::RunFailure;
use prompt_explore::model::scenario::ToolImplementation;
use prompt_explore::model::simulation::{RunExecution, RunPhase, RunProgress, Scenario, TraceTurn};
use prompt_explore::scenario::{ScenarioStore, runtime_for};
use prompt_explore::simulate::{RunnerOptions, SimulatorOptions, Workspace, WorkspaceToolLimits};
use serde_json::Value;
use subtle::ConstantTimeEq;
use utoipa::Modify;
use utoipa::openapi::security::{HttpAuthScheme, HttpBuilder, SecurityScheme};

mod evidence;
mod scenarios;

const MODEL: &str = "glm-5.2";
/// Preserve the investigation route's previous JSON-body allowance while also
/// allowing the separately configured compressed workspace maximum.
const INVESTIGATION_JSON_ALLOWANCE: usize = 8 * 1024 * 1024;
/// Multipart boundaries and part headers are outside both payloads. Keep an
/// explicit allowance so an archive exactly at its documented cap is accepted.
const MULTIPART_FRAMING_ALLOWANCE: usize = 1024 * 1024;

fn workspace_compressed_limit() -> usize {
    std::env::var("PROMPT_EXPLORE_WORKSPACE_COMPRESSED_LIMIT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(prompt_explore::simulate::workspace::DEFAULT_COMPRESSED_LIMIT)
}

fn workspace_decompressed_limit() -> usize {
    std::env::var("PROMPT_EXPLORE_WORKSPACE_DECOMPRESSED_LIMIT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(prompt_explore::simulate::workspace::DEFAULT_DECOMPRESSED_LIMIT)
}

fn investigation_body_limit() -> usize {
    workspace_compressed_limit()
        .saturating_add(INVESTIGATION_JSON_ALLOWANCE)
        .saturating_add(MULTIPART_FRAMING_ALLOWANCE)
}

struct AppState {
    client: Option<Arc<ProviderClient>>,
    jobs: Mutex<HashMap<String, Job>>,
    /// Reusable scenario definitions with their lifecycle rules (edit while
    /// unreferenced, pinned by investigations, fork/cascade to change).
    scenarios: Mutex<ScenarioStore>,
    /// Simulation probes, keyed by probe id. Probes never pin a scenario and
    /// never become frontier candidates.
    probes: Mutex<HashMap<String, scenarios::ProbeRecord>>,
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
    /// Live progress: populated as PUT model turns are simulated.
    progress: Arc<std::sync::Mutex<RunProgress>>,
    /// Wall-clock start and execution completion, epoch millis.
    started_at: u64,
    finished_at: Option<u64>,
    budget: Budget,
    assessment: Option<Assessment>,
    /// The run's free-form `reason` (advisory justification: what the
    /// run aims to accomplish, what changed vs. earlier runs, what a
    /// reader should know — no strict standard). Shown so a reader can
    /// read the unfolding traces with that framing in mind. Nothing is
    /// judged against it.
    reason: Option<String>,
    /// The prompt under test.
    put: PromptUnderTest,
    /// The input scenario (narrative, world_state, simulator_notes), so the
    /// ground truth is visible while the run unfolds. This is the narrative
    /// SNAPSHOT the reference pinned, not a live lookup.
    scenario: Scenario,
    /// The reusable scenario this run pinned, and the exact revision/hash it
    /// read. A simulation fix elsewhere never changes what this trace ran.
    scenario_id: String,
    scenario_revision: u64,
    scenario_definition_hash: String,
    /// Live per-role usage trackers, shared with the running task. Kept on the
    /// job so ANY read — the list view included — can report spend without
    /// fetching evidence. None when the server has no provider client, in which
    /// case the run failed before its first call and `result.usage` is zeroed.
    put_tracker: Option<Arc<UsageTracker>>,
    sim_tracker: Option<Arc<UsageTracker>>,
    /// The resolved model name running the prompt under test (the `put_model`
    /// from the request, or the server default). Stored so the dashboard
    /// can show which model produced the traces — set at job creation,
    /// visible while running.
    put_model: String,
    /// The resolved model name running the tool simulator (the `sim_model`
    /// from the request, or the server default). It resolves independently
    /// of `put_model`. The simulator is the test environment; surfacing it lets
    /// a reader judge whether it was powerful enough to render believably.
    sim_model: String,
    /// The resolved thinking level the PUT ran at (the request's
    /// `put_thinking_level`, or `None` = provider default). Recorded so
    /// a reader of the traces knows what produced them — a reasoning
    /// model's effort is part of its measured behavior.
    put_thinking_level: Option<ThinkingLevel>,
    /// The resolved thinking level the simulator ran at (the request's
    /// `sim_thinking_level`, or `None` = provider default). Falls back
    /// INDEPENDENTLY of `put_thinking_level` — omitting it never
    /// inherits the PUT's level.
    sim_thinking_level: Option<ThinkingLevel>,
    /// Resolved controls that governed the PUT and simulator conversations.
    /// Recorded with the job so its traces can be reproduced even when server
    /// environment defaults later change.
    conversation_controls: ResolvedConversationControls,
    /// How many files seeded the simulation workspace (0 if no zip was
    /// uploaded). Surfaced so a reader knows whether the simulator had a
    /// materialized world to consult, or answered purely from narrative.
    workspace_files: usize,
    /// Caller-graded axes on this job: axis name → number, PATCHed via
    /// PATCH /api/investigations/{id}. Never interpreted by the harness.
    grades: BTreeMap<String, f64>,
    /// Immutable provenance attributes plus caller-owned campaign attributes. Reserved
    /// provenance keys are set once when the job is created; callers may
    /// merge/delete only their own keys (notably the UI display attribute `label`).
    attributes: BTreeMap<String, String>,
}

#[derive(Clone, Copy, Serialize, PartialEq, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
enum JobStatus {
    Running,
    Done,
    Failed,
}

#[derive(Deserialize, Clone, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
struct InvestigateRequest {
    /// The scenario to run, BY REFERENCE. The referenced definition's tool
    /// contracts become this run's tool surface; the prompt under test supplies
    /// only its template and design goals.
    scenario_id: String,
    /// Optional guard against editing races: refuse to run unless the scenario
    /// is still at this revision. The referenced scenario is pinned by this
    /// submission, so it cannot be edited afterwards either.
    #[serde(default)]
    scenario_revision: Option<u64>,
    /// Optional explicit bindings for the scenario's declared inputs (replay a
    /// specific sample instead of drawing a new one). Supply every declared key
    /// or omit entirely.
    #[serde(default)]
    resolved_inputs: Option<HashMap<String, Value>>,
    investigation: Investigation,
    put: PromptUnderTest,
    /// Model for the prompt under test. Omit to use the server default
    /// (`glm-5.2`). Provider
    /// is selected by namespace prefix, e.g. `zai_coding::glm-5.2`,
    /// `open_router::deepseek/...`, `bedrock_sigv4::<model-id>`,
    /// `vertex::gemini-2.5-pro`; a bare
    /// name uses the server's default provider (`PROMPT_EXPLORE_PROVIDER`).
    /// See `GET /api/models` for available namespaced model strings.
    ///
    /// This is the model you are TESTING: when experimenting to find
    /// which model works well for your prompt, this is the one you vary
    /// across runs. Keep `sim_model` fixed while you do (see below), so
    /// candidates share simulator configuration, not fixed responses. Each run
    /// resolves inputs and renders tools afresh; inspect differences before
    /// attributing an outcome solely to the prompt/model.
    #[serde(default)]
    put_model: Option<String>,
    /// Migration only: the simulator's model now lives on the scenario.
    /// Supplying it is a 400 with that guidance, never a silent override.
    #[serde(default)]
    sim_model: Option<Value>,
    /// Thinking/reasoning level for the PUT runner ONLY (the agent
    /// under test): `none` | `minimal` | `low` | `medium` | `high` |
    /// `xhigh` | `max`. Omit to keep the provider's default (which is
    /// what runs today — no field is sent). This pins what you are
    /// measuring: a reasoning model's default effort is part of its
    /// measured behavior, and without this you can neither vary it
    /// (trade thoroughness for cost at `low`/`none`) nor tell two runs
    /// apart. The resolved value comes back on the job view as
    /// `put_thinking_level`. The SIMULATOR's level is the scenario's
    /// (`simulation.sim_thinking_level`), never inherited from this.
    #[serde(default)]
    put_thinking_level: Option<ThinkingLevel>,
    /// Migration only: the simulator's thinking level now lives on the
    /// scenario (`simulation.sim_thinking_level`).
    #[serde(default)]
    sim_thinking_level: Option<Value>,
    /// Per-investigation PUT conversation controls. Omit a field to use its
    /// documented default. Simulator settings belong to the scenario.
    #[serde(default)]
    conversation_controls: ConversationControls,
    /// Migration only: the inline scenario field this API no longer accepts.
    /// Present so supplying it produces an actionable error instead of an
    /// opaque unknown-field message.
    #[serde(default)]
    scenario: Option<Value>,
    /// Migration only: the old array form. Rejected with the same guidance.
    #[serde(default)]
    scenarios: Option<Value>,
    /// Caller-owned campaign attributes. This field is literally `attributes`;
    /// there is no `tags` alias and unknown fields are rejected. Keys use
    /// `^[a-z][a-z0-9_]{0,63}$`; values are strings up to 1024 UTF-8 bytes.
    /// `label` is the special editable
    /// display label shown by the UI. POST rejects every system-owned key:
    /// `put_model`/`sim_model` are the resolved provider-qualified model
    /// names; `put_thinking`/`sim_thinking` are a reasoning keyword or
    /// `provider_default`; `prompt_hash` is SHA-256 of canonical PUT
    /// template/tools/design_goals (not cosmetic PUT id); and
    /// `workspace_hash` is SHA-256 of sorted uploaded workspace path/content
    /// pairs (including the stable empty-workspace hash). `simulation_backend`
    /// is `llm` or `lua`; `step_budget` and `token_budget` are decimal limits
    /// (unbounded token budget is `unlimited`). All are immutable provenance;
    /// group by them explicitly when comparing execution configurations.
    #[serde(default)]
    attributes: BTreeMap<String, String>,
}

/// Caller-selected limits and sampling controls for an investigation's LLM
/// conversations. Defaults: temperature 0.7; PUT/simulator output limits
/// 32768 tokens each; 20 total JSON-reply attempts; 250 workspace turns;
/// 5000 read lines, 1000 grep matches, 2000 characters per grep line,
/// and 1 MiB constructed output per workspace tool call.
#[derive(Debug, Clone, Default, Deserialize, utoipa::ToSchema)]
struct ConversationControls {
    /// PUT sampling temperature; omit for the documented default.
    #[serde(default)]
    #[schema(minimum = 0)]
    put_temperature: Option<f32>,
    /// Maximum output tokens per PUT completion; omit for the documented default.
    #[serde(default)]
    #[schema(minimum = 1)]
    put_max_tokens: Option<u32>,
    /// Migration only: simulator sampling temperature now lives on the
    /// scenario definition (`simulation.temperature`). Supplying it here is a
    /// 400 with that guidance, never a silent override.
    #[serde(default)]
    sim_temperature: Option<Value>,
    /// Migration only: simulator output limit (`simulation.max_tokens`).
    #[serde(default)]
    sim_max_tokens: Option<Value>,
    /// Migration only: simulator repair budget
    /// (`simulation.max_repair_attempts`).
    #[serde(default)]
    sim_max_repair_attempts: Option<Value>,
    /// Migration only: simulator workspace turns
    /// (`simulation.max_workspace_turns`).
    #[serde(default)]
    max_workspace_turns: Option<Value>,
    /// Migration only: workspace read bound (`simulation.workspace.max_read_lines`).
    #[serde(default)]
    workspace_max_read_lines: Option<Value>,
    /// Migration only: workspace grep bound (`simulation.workspace.max_grep_matches`).
    #[serde(default)]
    workspace_max_grep_matches: Option<Value>,
    /// Migration only: grep line length (`simulation.workspace.max_line_len`).
    #[serde(default)]
    workspace_max_line_len: Option<Value>,
    /// Migration only: workspace output budget
    /// (`simulation.workspace.max_output_bytes`).
    #[serde(default)]
    workspace_max_output_bytes: Option<Value>,
}

/// Actual controls after request and server defaults have been resolved. The
/// PUT's controls come from the investigation; the simulator's come from the
/// referenced scenario.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
struct ResolvedConversationControls {
    put_temperature: Option<f32>,
    put_max_tokens: Option<u32>,
    /// The simulator settings this run used, resolved from the scenario.
    simulator: ResolvedSimulatorSettings,
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
struct ResolvedSimulatorSettings {
    sim_temperature: Option<f32>,
    sim_max_tokens: Option<u32>,
    sim_max_repair_attempts: usize,
    max_workspace_turns: usize,
    /// Lua sandbox limits, present exactly when the scenario supplies
    /// implementations (there is no enable switch: code supplied is code run).
    lua: Option<LuaOptions>,
    workspace_max_read_lines: usize,
    workspace_max_grep_matches: usize,
    workspace_max_line_len: usize,
    workspace_max_output_bytes: usize,
}

impl Default for ResolvedSimulatorSettings {
    fn default() -> Self {
        let simulator = SimulatorOptions::default();
        let workspace = WorkspaceToolLimits::default();
        Self {
            sim_temperature: simulator.temperature,
            sim_max_tokens: simulator.max_tokens,
            sim_max_repair_attempts: simulator.max_repair_attempts,
            max_workspace_turns: simulator.max_workspace_turns,
            lua: simulator.lua_simulation,
            workspace_max_read_lines: workspace.max_read_lines,
            workspace_max_grep_matches: workspace.max_grep_matches,
            workspace_max_line_len: workspace.max_line_len,
            workspace_max_output_bytes: workspace.max_output_bytes,
        }
    }
}

impl Default for ResolvedConversationControls {
    fn default() -> Self {
        let runner = RunnerOptions::default();
        Self {
            put_temperature: runner.put_temperature,
            put_max_tokens: runner.put_max_tokens,
            simulator: ResolvedSimulatorSettings::default(),
        }
    }
}

#[derive(Serialize, Clone, utoipa::ToSchema)]
struct InvestigateResponse {
    /// The completed evidence, if the scenario ran to a trace. Null when the
    /// run failed before a trace could be produced.
    #[schema(required = true)]
    trace: Option<TraceView>,
    /// Failure evidence, if the conversation failed (job status `failed`).
    /// Null when `trace` is present. There is no completed trace on failure:
    /// read `progress.turns`, `progress.resolved_inputs`, and
    /// `progress.simulation_program` on the job for evidence collected before
    /// the error. If a later tool call in one completion failed, the last
    /// progress turn may contain only that completion's successful exchanges.
    /// Do not discard those exchanges or setup artifacts.
    #[schema(required = true)]
    failure: Option<RunFailure>,
    /// Cumulative token usage and call counts, split by the prompt under test
    /// (`put`) and the simulator (`sim`). Present even when `failure` is set.
    usage: UsageByRole,
}

#[derive(Serialize, Clone, utoipa::ToSchema)]
struct TraceView {
    /// Deterministic termination, consumed budget and monotonic execution timing.
    /// A recorded trace need not contain a final PUT completion.
    execution: RunExecution,
    /// Structured PUT model turns, rendered as whole turn objects by the UI.
    /// Tool calls requested by one completion are nested together.
    turns: Vec<TraceTurn>,
    /// World state at the end of the trace (after all applied patches).
    final_world_state: HashMap<String, Value>,
    /// Number of tool calls the simulated PUT made in this trace.
    tool_calls: usize,
    /// The concrete {{variable}} values the simulator generated from
    /// the scenario's input_domain and rendered the template with — the
    /// exact input that produced this trace, for reproduction.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    resolved_inputs: HashMap<String, Value>,
    /// The scenario's caller-supplied Lua implementations, with the source
    /// hash each tool_exchanges[].lua_execution refers to. Empty when every
    /// response was rendered by the simulator LLM.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    implementations: Vec<ToolImplementation>,
}

#[derive(Serialize, utoipa::ToSchema)]
struct JobCreated {
    id: String,
    /// The stored provenance + caller attributes, including resolved model names
    /// and stable prompt/workspace hashes, available without a follow-up GET.
    attributes: BTreeMap<String, String>,
}

#[derive(Serialize, Clone, utoipa::ToSchema)]
/// One investigation. Deterministic execution evidence lives at
/// `progress.execution` while running, and at `result.trace.execution` once done
/// (the two are equal when the run is terminal); there is deliberately no
/// top-level `execution` alias. Read `stop_reason` there, not `status`, to learn
/// how the run ended: `done` only means a trace was recorded. For a token-capped
/// run the crossing completion is at `progress.execution.budget_cutoff_completion`
/// (fields `model_output`, `thinking`, `tool_calls` — not `content`), and for a
/// failure inside a tool batch `progress.execution.unrendered_call` names the
/// request whose response does not exist. Prefer GET /api/investigations/{id}/evidence
/// for reading the conversation itself.
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
    /// Which LLM phase the scenario is currently in (see RunPhase). This is
    /// the observable status of the job's LLM work.
    /// Mirrors `progress.phase`.
    phase: prompt_explore::model::simulation::RunPhase,
    started_at: u64,
    /// Execution completion epoch milliseconds, null while running. Core monotonic
    /// phase timings are in progress.execution; polling/file mtimes are not durations.
    #[schema(required = true)]
    finished_at: Option<u64>,
    /// Original per-conversation budget, retained even for failed/capped runs.
    budget: Budget,
    /// Caller-owned rationale, rubric and evidence references; never a harness verdict.
    #[schema(required = true)]
    assessment: Option<Assessment>,
    /// The run's free-form `reason` (advisory justification: what the
    /// run aims to accomplish, what changed vs. earlier runs, what a
    /// reader should know — no strict standard). Optional; surfaced to
    /// guide reading the traces. Nothing is judged against it.
    reason: Option<String>,
    /// The resolved model name that ran the prompt under test (the `put_model`
    /// from the request, or the server default). Echoed RESOLVED so a
    /// reader knows exactly what produced the traces — including the
    /// default, which the request leaves implicit.
    put_model: String,
    /// The resolved model name that ran the tool simulator (the `sim_model`
    /// from the request, or the server default), resolved independently of
    /// `put_model`. The simulator is the test ENVIRONMENT; a reader needs to
    /// see it to judge whether it was powerful enough to render the
    /// world believably.
    sim_model: String,
    /// The thinking level the PUT ran at (the request's
    /// `put_thinking_level`; absent = the provider's default). Part of
    /// the run's provenance: a reasoning model's effort changes cost
    /// and behavior, so a reader comparing traces needs to know it.
    #[serde(skip_serializing_if = "Option::is_none")]
    put_thinking_level: Option<ThinkingLevel>,
    /// The thinking level the simulator ran at (the request's
    /// `sim_thinking_level`; absent = the provider's default). Resolved
    /// independently of the PUT's — it never inherits it.
    #[serde(skip_serializing_if = "Option::is_none")]
    sim_thinking_level: Option<ThinkingLevel>,
    /// Resolved controls for the PUT and simulator conversations.
    conversation_controls: ResolvedConversationControls,
    /// How many files seeded the simulation workspace (0 = no zip upload;
    /// the simulator answered from narrative alone). The workspace is an
    /// in-memory filesystem the SIMULATOR consults via read/write/list_dir/
    /// grep — it is NOT the PUT's tools. See the endpoint description.
    workspace_files: usize,
    /// The reusable scenario this investigation pinned, with the exact revision
    /// and content hash it ran. A correction elsewhere never changes this trace;
    /// read the scenario to see whether a newer revision exists.
    scenario_id: String,
    scenario_revision: u64,
    scenario_definition_hash: String,
    /// Caller-graded axes on this investigation (PATCHed via
    /// PATCH /api/investigations/{id}). Free-form names, caller-chosen
    /// scales (0..1, 1..5, anything); the harness stores them and never
    /// interprets them.
    grades: BTreeMap<String, f64>,
    /// Immutable provenance plus caller-owned campaign attributes. `label` is the
    /// special editable display label; reserved provenance keys cannot change.
    attributes: BTreeMap<String, String>,
    /// The prompt under test.
    put: PromptUnderTest,
    /// The input scenario, by value: its narrative is the ground truth for
    /// interpreting the trace and progress.
    scenario: Scenario,
    /// Live progress for this scenario, populated while running and frozen
    /// when the job finishes. Lets a dashboard show a tool-call log as it
    /// happens. `progress.execution` is the deterministic run record (stop
    /// reason, counters, monotonic phase timings, and any unaccepted cutoff
    /// completion or unrendered tool call).
    progress: RunProgress,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<InvestigateResponse>,
}

#[derive(Serialize, Clone, utoipa::ToSchema)]
struct JobSummary {
    id: String,
    /// Null while running; epoch milliseconds when execution finished.
    #[schema(required = true)]
    finished_at: Option<u64>,
    /// Live/frozen deterministic execution counters and timing, not a quality grade.
    execution: RunExecution,
    status: JobStatus,
    /// Observable current LLM phase; never infer job work from bare `running`.
    phase: RunPhase,
    started_at: u64,
    /// Immutable provenance plus caller-owned campaign attributes, sufficient for
    /// a list view to group/filter before fetching full job evidence.
    attributes: BTreeMap<String, String>,
    /// Token usage and estimated cost, split by role. Live while the job runs,
    /// frozen once it finishes; identical to the detail view's `result.usage`,
    /// so a caller can total spend without fetching every job's evidence. Null
    /// only when the run never reached a provider (no configured client).
    usage: Option<UsageByRole>,
}

/// PATCH updates independently optional grades, attributes and assessment.
/// Every supplied value validates before anything is applied (atomic update).
/// Maps merge; assessment replaces as a whole, null clears, absent leaves unchanged.
#[derive(Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
struct InvestigationPatch {
    /// Axis name → number to set/overwrite, or null to delete. For prompt
    /// optimization, SEND these judgments after reading each run's evidence,
    /// not just into local files. Use the rubric agreed with the user, include
    /// assessment explaining the evidence, and confirm the PATCH response.
    /// Then read POST /api/frontier before the next prompt revision.
    #[serde(default)]
    grades: Option<BTreeMap<String, Option<f64>>>,
    /// Caller-owned attribute name → string to set/overwrite, or null to delete.
    /// This is `attributes`, never `tags` (unknown fields are rejected).
    /// `label` names the job in the UI. It affects group identity only when
    /// explicitly selected in `group_by`. System provenance keys are read-only.
    #[serde(default)]
    attributes: Option<BTreeMap<String, Option<String>>>,
    /// Caller-owned explanation and rubric with references to existing evidence.
    /// Omit to keep unchanged, object replaces entirely, JSON null clears.
    #[serde(default, deserialize_with = "deserialize_present_option")]
    assessment: Option<Option<Assessment>>,
}

fn deserialize_present_option<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

#[derive(Serialize, utoipa::ToSchema)]
struct InvestigationPatchView {
    grades: BTreeMap<String, f64>,
    attributes: BTreeMap<String, String>,
    #[schema(required = true)]
    assessment: Option<Assessment>,
}

#[derive(utoipa::OpenApi)]
#[openapi(
    info(
        title = "prompt-explore API",
        version = env!("CARGO_PKG_VERSION"),
        description = "Property-based testing for agent behavior. You author a reusable \
                       SCENARIO once (a test case: a world narrative, an input domain, a \
                       protagonist, the tool surface, and optional Lua implementations of those \
                       tools), develop and TEST its simulation with direct tool-call probes, \
                       and then run many investigations against it. Each investigation = one \
                       scenario revision + one prompt under test (PUT) + one conversation. The \
                       harness surfaces COMPLETE EVIDENCE — the world, input domain, resolved \
                       inputs, and the full trace of model turns — or explicit failure evidence. \
                       THE CALLER IS THE JUDGE: there is no in-harness verdict, and the optional \
                       `reason` is advisory framing for whoever reads the traces.

 \
                       PROMPT-OPTIMIZATION CHECKPOINT: READ -> PATCH -> COMPARE -> EDIT. \
                       When helping a developer improve a prompt, finish this checkpoint for the \
                       current results BEFORE editing the prompt or launching its successor. This \
                       is the experiment loop, not end-of-session reporting. Recording comparable \
                       judgments now reduces cherry-picking and lets the user independently inspect \
                       your comparison in the dashboard. Neither numbers nor a frontier prove quality.
 \
                       BEFORE RUNNING THE EXPERIMENT, agree what better means WITH the user: the \
                       quality axes, their scales, acceptable tradeoffs, which delivered outputs count, \
                       and the outcome of a NON-DELIVERING attempt (for example, empty output or a \
                       tool loop capped before delivering a review). If the user already supplied an \
                       acceptance rubric, use it; otherwise propose concrete metrics and ask for \
                       confirmation before calling them agreed. Do not wait for results to decide \
                       whether non-deliveries count, or silently omit them from the recommendation. \
                       Keep the rubric stable and store it in assessment.rubric; use measured \
                       put_cost_usd or elapsed_ms alongside quality, not instead of it.
 \
                       Example caller-owned rubric for an audit with known positive findings: \
                       delivered_recall = real actionable paths reported in the delivered review / \
                       known true paths; no delivered findings means 0. Precision = real warnings / \
                       emitted warnings; with no warnings it is undefined, not automatically 0 or 1. \
                       Optionally track usable_report separately (1 for a usable review, otherwise 0, \
                       under an agreed definition; nonempty text alone is not sufficient). Two \
                       comparable completed attempts, one with full delivered recall and one with \
                       no review, have mean delivered_recall 0.5, not 1.0. A genuinely ungradable \
                       trace, such as one with unreliable simulation evidence, instead needs an \
                       explanatory assessment and absent grades, not an invented failure score.
 \
                       For EACH completed investigation: (1) GET /api/investigations/{id}/evidence \
                       and read the actual conversation, stop reason and simulation fidelity. \
                       (2) Judge it against the agreed rubric. (3) Immediately SEND \
                       PATCH /api/investigations/{id} with grades AND an assessment explaining the \
                       evidence. Read the successful response and confirm it echoes your annotations \
                       (or re-GET to verify). Do not postpone PATCH until all prompt versions are \
                       finished. Scores in local JSON, notebook tables, a generated PATCH script, \
                       state_patch inside a simulated tool, and a promise to grade later do NOT \
                       record caller judgment on an investigation. If evidence cannot justify a \
                       grade, PATCH an assessment explaining why and clear any stale grade with null \
                       instead of manufacturing a score. Grade delivered behavior, not a proposed \
                       answer in thinking or a plausible final sentence alone.
 \
                       Then SEND POST /api/frontier and READ its returned points before the next \
                       prompt edit. Example request: \
                       {\"group_by\":[\"put_model\",\"put_thinking\",\"prompt_hash\"],\"axes\":[{\"name\":\"delivered_recall\",\"better\":\"higher\"},{\"name\":\"put_cost_usd\",\"better\":\"lower\"}]}. \
                       Every requested axis must exist on a run for it to contribute. Adding \
                       undefined precision to this primary comparison would exclude the no-review \
                       attempt again: inspect precision in a separate, explicitly conditional view \
                       and report its denominator. Execution-failed jobs remain excluded even when \
                       graded; account for them explicitly in the recommendation too. \
                       Inspect points[].values, included, excluded, preliminary, on_frontier and \
                       dominated_by. Missing requested grades remain explicit backlog; measured-only \
                       frontiers need no grades but cannot establish quality. Pending means no common \
                       contributing cohort; preliminary means some members were excluded. Neither \
                       means the candidate failed or won. Check corpus coverage and comparable \
                       provenance, not just the non-dominated marker. Explain the observed tradeoff \
                       and exclusions to the user, THEN choose a prompt change targeting a demonstrated \
                       weakness. Re-run comparable scenarios and repeat this checkpoint.
 \
                       Before saying an iteration is complete, check what actually happened: \
                       PATCH response confirmed? Frontier response read AFTER those judgments? \
                       Decision tied to its values and evidence? If not, do the missing calls now, \
                       or report the concrete blocker rather than claiming a validated improvement. \
                       This is caller workflow guidance, not a server-enforced gate; the API also \
                       supports exploratory trace reading without grades.
 \
                       WORKED LOOP (the caller does every judgment):
 \
                       1. POST /api/scenarios registers the world: `world`, `input_domain`, \
                       `user_message`, `simulator_notes`, `tools[]` (name/description/parameters/ \
                       side_effect, plus optional `lua_source`), and `simulation` settings (the \
                       simulator model, thinking level, limits, and Lua sandbox limits). Supply \
                       the initial workspace once, as the optional `workspace` .zip part of a \
                       multipart body (part `request` = the JSON, part `workspace` = the archive). \
                       The response `id` + `revision` are what you reference afterwards.
 \
                       2. Develop the simulation BEFORE spending investigations. POST \
                       /api/scenarios/{id}/simulations with an ordered `tool_calls` list; poll \
                       GET /api/scenarios/{id}/simulations/{probe_id}. Each call is rendered \
                       through the SAME engine an investigation uses (same argument validation, \
                       same Lua sandbox and rollback, same LLM delegation), so what you test is \
                       what runs. Read every call's `response`, `lua_execution` (computed vs \
                       delegated vs errored, with `source_hash`) and `workspace_ops`. Calls in one \
                       submission run in sequence in one session, so write/read consistency is \
                       testable; each submission starts from a fresh snapshot. A probe never \
                       invokes the PUT and never becomes a frontier candidate. You need no local \
                       Lua toolchain: the server parses, sandboxes, and executes the source.
 \
                       3. Iterate: PATCH /api/scenarios/{id} with the complete new definition plus \
                       the `expected_revision` you read (a stale value is refused). Editing is \
                       allowed exactly while NO investigation references the scenario; you need \
                       no separate publish step — submitting an investigation pins it.
 \
                       4. Check inputs too. A probe may pass `resolved_inputs` to pin the inputs \
                       for one test; omit it to sample them. Every run reports what it actually \
                       used in `resolved_inputs`.
 \
                       5. POST /api/investigations with `scenario_id` (plus optional \
                       `scenario_revision` as a staleness guard and optional `resolved_inputs`), \
                       `put` (template, design goals), the PUT's model/thinking level, and the \
                       investigation's budget/reason/attributes. The tool surface comes from the \
                       scenario: `put.tools` is rejected here rather than silently ignored. \
                       Submission pins the scenario immediately and atomically.
 \
                       6. Poll GET /api/investigations/{id}, then read \
                       GET /api/investigations/{id}/evidence. Read execution.stop_reason, budget \
                       and timing first: done means a trace was recorded, not necessarily a final \
                       answer. Read turns in order, including EVERY tool_exchanges[].call AND \
                       response. The response is the PUT observation; workspace_ops is only \
                       supporting provenance. lua_execution=computed means code executed, NOT \
                       that the response is faithful. A final correct answer can hide invalid \
                       root listings, false-empty searches, or invented files.
 \
                       7. Record the judgment in the product. This is the step that makes the run \
                       comparable on judged axes: immediately PATCH the id with `grades` (one number per agreed axis) and \
                       `assessment` (summary, the `rubric` scale, and `evidence` entries naming the \
                       turn/exchange the judgment rests on). Example: \
                       {\"grades\":{\"found_all\":0.75,\"precision\":1.0},\"assessment\":{\"summary\":\"Found three of four planted paths; missed the sanitizer bypass\",\"rubric\":\"found_all: share of planted paths reported, 0..1; precision: 1 - share of reported paths that are not real\",\"evidence\":[{\"turn\":1,\"exchange\":0,\"note\":\"cleared the highlight helper after reading stripTags; the surviving unclosed-tag payload never appears in the trace\"}]}}. \
                       Clear stale grades in the SAME PATCH when an assessment invalidates them \
                       (grades:{\"found_all\":null}). Grades are caller-owned; the harness stores and \
                       compares them and NEVER substitutes a verdict of its own. \
 \
                       8. Fixing a simulation after traces exist: the scenario is pinned, so \
                       POST /api/scenarios/{id}/fork (optionally with a `correction` note naming \
                       the predecessor). The fork is editable and shares the initial workspace \
                       without re-uploading it. Every investigation reports scenario_id, \
                       scenario_revision and scenario_definition_hash, so you can tell exactly \
                       which traces ran the old definition and re-run only those.
 \
                       9. READ THE FRONTIER BEFORE YOU CHANGE THE PROMPT. POST /api/frontier with \
                       `group_by` naming the variables you are comparing (put_model, put_thinking, \
                       prompt_hash, or your own labels such as a prompt-version attribute; the reserved \
                       provenance attributes also include scenario_id/scenario_revision/scenario_hash, \
                       simulation_backend, step_budget and token_budget) and `axes` naming the metrics you \
                       agreed with your user (your judged grades plus measured ones like put_cost_usd, \
                       steps_per_trace_avg, elapsed_ms). This is the corpus-wide table of what you \
                       compared: inspect the returned `points`, not a nonexistent `groups` field. \
                       A point with null `values` has no contributing run with every requested axis. \
                       Inspect `excluded` for missing grades, running/failed runs or unavailable \
                       measured axes. Preliminary points participate in dominance but their \
                       exclusions limit the conclusion; missing grades are a review backlog, not a loss. All stored jobs \
                       remain candidates; a card filter never limits candidacy. The caller owns corpus \
                       comparability and grade scales. After reading it — and only then — change the \
                       prompt. Share state via URL-encoded query values (group_by, axes, attributes) — \
                       never a bearer token. \
 \
                       Archiving: scenarios, investigations, grades and probes are all in \
                       memory and lost on restart. GET /api/scenarios/{id}/workspace exports the \
                       initial workspace inventory; a hash alone is not a reproducible workspace.
 \
                       DESIGN INTENT — why it works this way:
 \
                       • Scenarios are world SPECIFICATIONS, not instantiated data. A narrative \
                       pins what exists (inventory; facts, including NEGATIVE facts; completeness \
                       assertions; rendering rules) and the simulator lazily renders concrete \
                       tool responses from it. Materializing a full environment requires a closed \
                       world (enumerable, bounded, copyable); open worlds — web search, email, a \
                       payment network — can never be materialized, so a narrative is the only \
                       mechanism that generalizes. The optional workspace .zip is the container \
                       form of a closed world: upload it once with the scenario to hand the \
                       simulator authoritative bytes.
 \
                       • Tool responses are SIMULATED from the narrative. By default the \
                       simulator LLM renders every response. A tool with `lua_source` is tried \
                       in the sandbox FIRST: a computed reply costs no model call, \
                       `PleaseSimulateException(\"reason\")` delegates that one input, and a \
                       runtime error delegates too while keeping distinct error evidence and \
                       discarding its staged workspace writes. Code supplied is code run — there \
                       is no enable switch, and the harness never authors, repairs or rewrites \
                       the source. Code that executed is NOT proof it is faithful: read the \
                       responses against the world.
 \
                       • The answer to simulation unreliability is TRANSPARENCY, not \
                       enforcement: every response is in the trace, the same narrative is \
                       visible, and a response that contradicts the stated facts is visible for \
                       the caller to catch. When simulation quality is insufficient, sharpen the \
                       narrative, fix the implementation, or use a stronger simulator model \
                       (which is set on the SCENARIO, because the environment is part of the \
                       test case).
 \
                       • Reports clearly separate what was deterministic (validation, state \
                       patches, workspace bytes, usage, timing) from what was semantic (model \
                       output, rendered tool responses). The caller judges the semantic part.
 \
                       • Phases: progress.phase is resolving_inputs or put_loop. There is no \
                       preparation phase; nothing is compiled or generated during a run.
 \
                       THE SIMULATION WORKSPACE (optional, closed-world materialization). The \
                       optional `workspace` .zip is decompressed ENTIRELY IN MEMORY (never on \
                       disk) and seeds an in-memory filesystem the tool SIMULATOR consults. The \
                       simulator accesses it with four tools — read, write, list_dir, grep. The \
                       workspace is EPHEMERAL and per-run (every run gets a fresh copy of the \
                       scenario's seed; the agent under test never sees it — only tool \
                       responses). WHEN the simulator uses it is the world narrative's policy: \
                       state what the archive contains, where things live, and its completeness \
                       stance (closed: \"these are ALL the files\"; partial: \"these are SOME \
                       files; simulate the rest\"). Each tool exchange records the simulator's \
                       workspace operations (`workspace_ops`) so you can judge whether an \
                       answer was grounded in the uploaded files or invented. Caps: ≤ 50 MB \
                       compressed, ≤ 500 MB decompressed (overridable via \
                       PROMPT_EXPLORE_WORKSPACE_{COMPRESSED,DECOMPRESSED}_LIMIT); zip-slip \
                       entries and files in the reserved .prompt-explore namespace are rejected. \
 \
                       WRITING A LUA IMPLEMENTATION. `GET /docs/lua` serves the handler \
                       reference from this server: the chunk contract, every \
                       `ctx.workspace` operation with its argument and return shape, the sandbox \
                       limits, and how a declined call, a runtime error and a missing implementation \
                       each delegate a single call to the simulator LLM. The \
                       `LuaWorkspaceCapability` schema states the same operations in the spec, \
                       and `POST /api/scenarios/{id}/simulations` tests them before you spend an \
                       investigation. After a run, `execution.lua_computed_calls` / \
                       `lua_fallback_calls` / `lua_error_calls` say whether your code actually \
                       served it.
 \
                       AUTHENTICATION. The server is open by default. When \
                       PROMPT_EXPLORE_API_TOKEN is set (non-empty), every /api/* \
                       route EXCEPT /api/openapi.json requires an `Authorization: \
                       Bearer <token>` header (security scheme `api_token`). The \
                       web UI prompts for the token and stores it in localStorage."
    ),
    modifiers(&SecurityAddon),
    components(schemas(LuaWorkspaceCapability)),
    paths(
        index,
        lua_docs,
        list_investigations,
        create_investigation,
        get_investigation,
        evidence::get_evidence,
        patch_investigation,
        delete_investigation,
        scenarios::list_scenarios,
        scenarios::create_scenario,
        scenarios::get_scenario,
        scenarios::patch_scenario,
        scenarios::fork_scenario,
        scenarios::delete_scenario,
        scenarios::get_scenario_workspace,
        scenarios::create_probe,
        scenarios::list_probes,
        scenarios::get_probe,
        frontier,
        list_models
    )
)]
struct ApiDoc;

/// Documentation-only schema: the sandbox capability a Lua handler receives as
/// `ctx`. It never appears in a payload; it exists so the spec states the
/// handler's available operations, their shapes, and the two ways a handler can
/// hand a call back to the simulator LLM. The full prose reference is served at
/// `GET /docs/lua`.
#[derive(serde::Serialize, utoipa::ToSchema)]
#[allow(dead_code)]
struct LuaWorkspaceCapability {
    /// The chunk must RETURN a function taking `(args, ctx)` and returning
    /// `{response = <the value the prompt under test receives>, state_patch =
    /// <write tools only>}`. `response` is required, and should match the tool's
    /// declared contract shape exactly.
    handler_contract: String,
    /// `ctx.workspace.list_dir({path?})` — direct children only. Returns
    /// `{path, entries:[{name, kind:"file"|"dir"}], truncated}`; unknown path
    /// returns `{path, error:"not found"}`. Omit `path` (or pass "" or ".") for
    /// the workspace root.
    workspace_list_dir: String,
    /// `ctx.workspace.read({path, start_line?, end_line?})` — a file's contents.
    /// Returns `{path, content, start_line, end_line, total_lines, truncated}`;
    /// a missing file returns `{path, error:"not found"}`. Lines are 1-based;
    /// at most `workspace_max_read_lines` are returned.
    workspace_read: String,
    /// `ctx.workspace.grep({pattern, path?, case_insensitive?})` — a LITERAL
    /// substring search (never a regex). Returns `{pattern, matches:[{path,
    /// line, text}], truncated}`; at most `workspace_max_grep_matches` matches.
    workspace_grep: String,
    /// `ctx.workspace.write({path, content})` — writes into the run's PRIVATE
    /// overlay (visible to later calls in this run only, never to another run
    /// and never to the prompt under test except through `response`). Returns
    /// `{path, bytes, ok:true}`.
    workspace_write: String,
    /// `PleaseSimulateException("reason")` declines this call: the simulator LLM
    /// renders the response from the world narrative instead. Recorded as
    /// `lua_execution.outcome = "fallback"`. Declining is a normal outcome.
    decline_to_simulator: String,
    /// A runtime error or breached sandbox limit also delegates that one call to
    /// the simulator LLM, with `lua_execution.outcome = "error"`; writes the
    /// handler staged are discarded first. A tool with no implementation
    /// produces no `lua_execution` record at all. Read
    /// `execution.lua_computed_calls` / `lua_fallback_calls` / `lua_error_calls`
    /// after a run to see whether your code served it.
    errors_and_limits_delegate: String,
    /// The prose reference for all of this, also served at GET /docs/lua.
    reference_url: String,
}

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
    println!("    PROMPT_EXPLORE_MAX_WORKSPACE_TURNS");
    println!("                           Default maximum workspace tool calls per simulator");
    println!("                           response (default: 250; request override available).");
    println!("    PROMPT_EXPLORE_MAX_RETRIES");
    println!(
        "                           Retries per provider completion (default: 20; 0 disables)."
    );
    println!(
        "                           Covers transient 429s, 408/5xx, connection/response failures."
    );
    println!("                           Permanent auth/validation/quota errors fail fast.");
    println!("                           Retry-After can extend the configured backoff.");
    println!("    PROMPT_EXPLORE_RETRY_BASE_DELAY_MS");
    println!("                           Linear retry backoff step in ms (default: 5000).");
    println!("    PROMPT_EXPLORE_STREAMING");
    println!("                           Stream provider replies and bound each attempt by idle");
    println!("                           time instead of total time (default: 1). Set to 0 to use");
    println!("                           the blocking call and the total deadline below.");
    println!("    PROMPT_EXPLORE_STREAM_IDLE_MS");
    println!("                           No streamed output for this long counts as a stalled");
    println!("                           attempt and is retried (default: 120000). Every event -");
    println!("                           content, reasoning, tool call, heartbeat - resets it, so");
    println!("                           a slow answer is never cancelled; 0 disables the bound.");
    println!("    PROMPT_EXPLORE_REQUEST_TIMEOUT_MS");
    println!(
        "                           Blocking-only total per-attempt deadline (default: 60000;"
    );
    println!("                           used when PROMPT_EXPLORE_STREAMING=0); 0 disables it.");
    println!("    PROMPT_EXPLORE_RETRY_JITTER_PERCENT");
    println!("                           Maximum positive retry jitter (default: 10 percent).");
    println!("    PROMPT_EXPLORE_WORKSPACE_COMPRESSED_LIMIT");
    println!("                           Maximum size (bytes) of uploaded workspace .zip files");
    println!("                           (default: 52428800 = 50 MB).");
    println!("    PROMPT_EXPLORE_WORKSPACE_DECOMPRESSED_LIMIT");
    println!("                           Maximum total decompressed size (bytes) of workspace");
    println!("                           contents (default: 524288000 = 500 MB).");
}

/// The full application router. Factored out of `main` so tests (and
/// anything else embedding the server) get the EXACT production stack —
/// routing, auth, body limits, middleware — not a parallel one.
fn build_app(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/docs/lua", get(lua_docs))
        .route("/openapi.json", get(openapi_json))
        .route("/vendor/preact.mjs", get(vendor_preact))
        .route("/vendor/hooks.mjs", get(vendor_hooks))
        .route("/vendor/htm.mjs", get(vendor_htm))
        .route(
            "/api/investigations",
            get(list_investigations)
                .post(create_investigation)
                // The archive itself is checked again by
                // `unpack_zip_with_limits`; this outer cap must leave room for
                // the documented compressed maximum plus JSON and framing.
                .route_layer(DefaultBodyLimit::max(investigation_body_limit())),
        )
        .route(
            "/api/investigations/{id}",
            get(get_investigation)
                .patch(patch_investigation)
                .delete(delete_investigation),
        )
        .route(
            "/api/investigations/{id}/evidence",
            get(evidence::get_evidence),
        )
        .route(
            "/api/scenarios",
            get(scenarios::list_scenarios)
                .post(scenarios::create_scenario)
                .route_layer(DefaultBodyLimit::max(investigation_body_limit())),
        )
        .route(
            "/api/scenarios/{id}",
            get(scenarios::get_scenario)
                .patch(scenarios::patch_scenario)
                .delete(scenarios::delete_scenario)
                .route_layer(DefaultBodyLimit::max(investigation_body_limit())),
        )
        .route(
            "/api/scenarios/{id}/fork",
            post(scenarios::fork_scenario)
                .route_layer(DefaultBodyLimit::max(investigation_body_limit())),
        )
        .route(
            "/api/scenarios/{id}/workspace",
            get(scenarios::get_scenario_workspace),
        )
        .route(
            "/api/scenarios/{id}/simulations",
            get(scenarios::list_probes).post(scenarios::create_probe),
        )
        .route(
            "/api/scenarios/{id}/simulations/{probe_id}",
            get(scenarios::get_probe),
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

/// A DONE job fabricated for tests and the `--demo-frontier` mode: one
/// scenario with a trace of `steps` completions and a PUT that burned
/// `out_tokens` output tokens. Realistic shapes, made-up numbers (clearly a
/// fixture — no LLM was billed).
#[allow(dead_code)]
fn fabricate_done_job(
    job_id: &str,
    put_id: &str,
    template: &str,
    out_tokens: u64,
    steps: usize,
) -> (String, Job) {
    let scenario = Scenario {
        world: "Demo world: order O-1 exists and belongs to the user. Facts: \
                the user has NOT asked to cancel anything."
            .into(),
        input_domain: HashMap::new(),
        user_message: Some("yes".into()),
        simulator_notes: String::new(),
    };
    let trace = TraceView {
        execution: RunExecution {
            stop_reason: Some(prompt_explore::model::simulation::RunStopReason::FinalCompletion),
            steps_used: steps as u64,
            ..Default::default()
        },
        turns: vec![
            TraceTurn {
                model_output: "Order O-1 is confirmed cancelled.".into(),
                thinking: None,
                tool_exchanges: vec![],
            };
            steps
        ],
        final_world_state: HashMap::new(),
        tool_calls: 0,
        resolved_inputs: HashMap::new(),
        implementations: Vec::new(),
    };
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
    (
        job_id.to_string(),
        Job {
            status: JobStatus::Done,
            result: Some(InvestigateResponse {
                trace: Some(trace),
                failure: None,
                usage,
            }),
            progress: Arc::new(Mutex::new(RunProgress::default())),
            put_tracker: None,
            sim_tracker: None,
            started_at: 0,
            finished_at: Some(0),
            budget: Budget {
                max_steps_per_trace: steps as u32,
                max_tokens: None,
            },
            assessment: None,
            reason: Some(
                "Tone-instruction sweep: comparing politeness vs. cost on the same scenarios."
                    .into(),
            ),
            put: PromptUnderTest {
                id: put_id.into(),
                template: template.into(),
                tools: vec![],
                design_goals: "Cancel orders only on explicit user request.".into(),
            },
            grades: BTreeMap::new(),
            scenario,
            scenario_id: "scn-demo".into(),
            scenario_revision: 1,
            scenario_definition_hash: "demo".into(),
            put_model: "zai_coding::glm-5.2".into(),
            sim_model: "zai_coding::glm-5.2".into(),
            put_thinking_level: None,
            sim_thinking_level: None,
            conversation_controls: ResolvedConversationControls::default(),
            workspace_files: 0,
            attributes: system_attributes(
                "zai_coding::glm-5.2",
                "zai_coding::glm-5.2",
                None,
                None,
                &PromptUnderTest {
                    id: put_id.into(),
                    template: template.into(),
                    tools: vec![],
                    design_goals: "Cancel orders only on explicit user request.".into(),
                },
                &attributes::workspace_hash(&Workspace::empty()),
                BTreeMap::new(),
            ),
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
        scenarios: Mutex::new(ScenarioStore::new()),
        probes: Mutex::new(HashMap::new()),
        default_provider: provider.clone(),
        models_client: prompt_explore::llm::GenaiClient::builder()
            .build()
            .expect("failed to initialize model-listing HTTP client"),
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

/// Models available to put in a request's `put_model` or `sim_model` field, by provider.
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
/// This does NOT call generation endpoints or check credit balance: available
/// means catalog/configuration discovery, not usable inference. Smoke-test one
/// small investigation with both chosen roles before a corpus fanout. A 429 can
/// mean exhausted balance rather than transient rate limiting; inspect the error,
/// fix provider funding/permissions, and do not silently switch the simulator.
#[derive(Serialize, Clone, utoipa::ToSchema)]
struct ModelsResponse {
    /// Model used when a request omits `put_model` (a bare name; the server
    /// resolves it via `server_default_provider`).
    server_default_model: String,
    /// Provider applied to bare model names when no namespace is given
    /// (from PROMPT_EXPLORE_PROVIDER). Maps to a namespace prefix:
    /// `zai` -> `zai_coding::`, `zai_standard` -> `zai::`,
    /// `openrouter` -> `open_router::`, `bedrock` -> `bedrock_sigv4::`,
    /// `gemini` -> `vertex::`.
    server_default_provider: String,
    /// Always false: this endpoint lists catalogs/configuration, never makes a
    /// charged generation call or checks credit balance. `available` is NOT a
    /// readiness guarantee. Run one small investigation with both chosen roles
    /// before fanout; quota/balance failures need provider/operator action.
    generation_checked: bool,
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
        generation_checked: false,
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

#[utoipa::path(
    get,
    path = "/docs/lua",
    responses((status = 200, description = "Lua handler reference (markdown): the chunk contract, the ctx.workspace operations with their argument and return shapes, the sandbox limits, and how delegation to the simulator LLM works", content_type = "text/markdown"))
)]
async fn lua_docs() -> impl axum::response::IntoResponse {
    // The Lua handler reference travels INSIDE the binary, so the API is
    // self-describing: a caller whose only manual is /openapi.json can still
    // learn the handler contract and the ctx.workspace capability. Served as
    // markdown on purpose — it is prose for a model to read, not a data type.
    (
        [
            (
                axum::http::header::CONTENT_TYPE,
                "text/markdown; charset=utf-8",
            ),
            (axum::http::header::CACHE_CONTROL, "no-cache"),
        ],
        LUA_API_MD,
    )
        .into_response()
}

/// Serve the web UI. Share a view using URL-encoded JSON query parameters:
/// `group_by` is an array of attribute names, `axes` an array of {name,better},
/// and `attributes` an object of exact string matches for cards ONLY. Filtering
/// cards never changes the all-jobs frontier. The UI copies/restores these view
/// settings without storing a server-side selection. Never put tokens in URLs.
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
    // Always send no-cache. The page is embedded in the binary, and a
    // restart with UI changes serves a DIFFERENT page at the same URL —
    // without this header the browser's heuristic cache happily keeps
    // serving the old shell (observed: new job-card fields invisible
    // until a hard refresh). `no-cache` still caches but forces
    // revalidation, which without an ETag is a cheap 40 KB re-fetch on
    // a local tool.
    (
        [(axum::http::header::CACHE_CONTROL, "no-cache")],
        Html(html),
    )
        .into_response()
}

/// Start an investigation: run its required scenario against the PUT and
/// surface the resulting trace or failure evidence. There is no judge — the
/// caller reads the trace and judges. Runs in the background; poll the returned
/// id. The result includes token usage even on failure.
///
/// One investigation is one conversation: send `scenario`, not `scenarios`.
/// For different worlds/workspaces or repeated samples, submit independent
/// investigations and group them using attributes. Each repeated submission
/// resolves inputs and simulates afresh; it does not isolate PUT variability
/// with fixed inputs/tool responses. There is no sample-count field, batch
/// endpoint, or reusable workspace handle. Every upload is independent.
///
/// Two request shapes are accepted:
/// - `application/json` — the body is an `InvestigateRequest` (no workspace).
/// - `multipart/form-data` — TWO parts: a `request` part whose body is the
///   `InvestigateRequest` JSON, and an OPTIONAL `workspace` part whose body
///   is a `.zip` archive. The zip is decompressed ENTIRELY IN MEMORY (never
///   written to disk) and seeds the SIMULATION WORKSPACE — an in-memory
///   filesystem the tool SIMULATOR consults with four tools (read, write,
///   list_dir, grep). Hard caps: the compressed zip must be ≤ 50 MB and
///   decompress to ≤ 500 MB total (overridable via
///   PROMPT_EXPLORE_WORKSPACE_{COMPRESSED,DECOMPRESSED}_LIMIT), or the
///   request is rejected. Zip entries
///   that escape the workspace root (zip-slip), or use the reserved private
///   `.prompt-explore` namespace, are rejected.
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
/// the simulator's workspace operations appear in each trace tool exchange
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
                summary = "A cancellation tool, one scenario, no model overrides",
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
                    "scenario": {
                        "user_message": "yes",
                        "world": "Inventory: order O-1 exists and belongs to the user; cancel_order cancels an order. Facts: the user has NOT asked to cancel anything; the ONLY user turn is the word 'yes', given before any question. Completeness: that is the entire conversation. Rendering: refuse anything outside the inventory; filler introduces no new facts; never contradict the facts."
                    }
                })
            )
        ))
    ),
    security(("api_token" = [])),
    responses(
        (status = 202, description = "Investigation job created", body = JobCreated),
        (status = 400, description = "Malformed request body (including legacy `scenarios`), invalid conversation controls, invalid/oversized zip, or a thinking level on a model for which the adapter has no reasoning mapping (e.g. Bedrock Meta). Model-specific unsupported keywords are instead rejected by the provider during execution; poll the job and inspect result.failure."),
        (status = 401, description = "Missing or invalid bearer token")
    )
)]
async fn create_investigation(State(state): State<Arc<AppState>>, req: Request) -> Response {
    let limit = 16 * 1024 * 1024;
    let bytes = match to_bytes(req.into_body(), limit).await {
        Ok(bytes) => bytes,
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(
                    serde_json::json!({ "error": format!("could not read request body: {error}") }),
                ),
            )
                .into_response();
        }
    };
    let investigate_req: InvestigateRequest = match serde_json::from_slice(&bytes) {
        Ok(request) => request,
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": format!(
                        "body is not valid InvestigateRequest JSON: {error}. An investigation now \
                         names its world: send {{\"scenario_id\": \"<id from POST /api/scenarios>\", \
                         \"put\": {{\"id\", \"template\", \"design_goals\"}}, \"investigation\": \
                         {{\"budget\": ...}}}}; the tool surface comes from the scenario"
                    )
                })),
            )
                .into_response();
        }
    };

    // Fail fast on migrations and on a thinking level the adapter would ignore.
    if let Some(err) = migrated_request_field(&investigate_req)
        .or_else(|| thinking_level_problem(&investigate_req, &state.default_provider))
        .or_else(|| conversation_controls_problem(&investigate_req.conversation_controls))
        .or_else(|| validate_post_attributes(&investigate_req.attributes).err())
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": err })),
        )
            .into_response();
    }

    // Resolve the scenario and pin it in ONE critical section: an accepted
    // investigation must never race an edit, and a rejected one must not lock
    // the scenario. The job record and the reference are inserted under the
    // same store lock (deletion takes the same locks in the same order), so a
    // concurrent delete either sees the reference and refuses, or wins before
    // this submission reads the record at all.
    let mut store = state.scenarios.lock().unwrap();
    let (runtime, scenario_snapshot, scenario_revision, scenario_definition_hash) = {
        let Some(record) = store.get(&investigate_req.scenario_id) else {
            return scenarios::store_error_response(
                prompt_explore::scenario::StoreError::NotFound(investigate_req.scenario_id.clone()),
            );
        };
        if let Some(expected) = investigate_req.scenario_revision {
            if expected != record.revision {
                return scenarios::store_error_response(
                    prompt_explore::scenario::StoreError::StaleRevision {
                        id: investigate_req.scenario_id.clone(),
                        expected,
                        current: record.revision,
                    },
                );
            }
        }
        let workspace_limits = scenario_workspace_limits(&record.definition.simulation);
        let runtime = runtime_for(
            record,
            record.workspace.clone().with_tool_limits(workspace_limits),
        );
        (
            runtime,
            record.definition.scenario(),
            record.revision,
            record.definition_hash.clone(),
        )
    };

    // The prompt under test supplies its template and design goals; the tool
    // surface is the scenario's.
    let mut put = investigate_req.put.clone();
    put.tools = runtime.tools.clone();

    let id = spawn_investigation(
        state.clone(),
        &investigate_req,
        put,
        scenario_snapshot,
        scenario_revision,
        scenario_definition_hash,
        runtime,
        Some(&mut store),
    );
    drop(store);
    let attributes = state.jobs.lock().unwrap()[&id].attributes.clone();
    (StatusCode::ACCEPTED, Json(JobCreated { id, attributes })).into_response()
}

/// Resolve the PUT's model (the only model this request owns; the simulator's
/// is the scenario's).
fn resolved_models(req: &InvestigateRequest) -> (String, String) {
    (
        req.put_model.clone().unwrap_or_else(|| MODEL.into()),
        MODEL.into(),
    )
}

/// Fail fast when the adapter would silently ignore a thinking level. Only the
/// PUT's level is request-owned here; the scenario's simulator level is checked
/// when the probe or run resolves it.
fn thinking_level_problem(req: &InvestigateRequest, default_provider: &str) -> Option<String> {
    let (put_model, _) = resolved_models(req);
    let mut problems = Vec::new();
    if let Some(level) = req.put_thinking_level {
        let qualified = prompt_explore::llm::qualify_model(&put_model, default_provider);
        if let Err(e) = prompt_explore::llm::thinking_level_supported(&qualified) {
            problems.push(format!(
                "put_thinking_level {level:?} on '{put_model}': {e}"
            ));
        }
    }
    (!problems.is_empty()).then(|| problems.join("; "))
}

fn conversation_controls_problem(controls: &ConversationControls) -> Option<String> {
    let mut problems = Vec::new();
    for (name, supplied, guidance) in [
        (
            "sim_temperature",
            controls.sim_temperature.is_some(),
            "simulation.temperature",
        ),
        (
            "sim_max_tokens",
            controls.sim_max_tokens.is_some(),
            "simulation.max_tokens",
        ),
        (
            "sim_max_repair_attempts",
            controls.sim_max_repair_attempts.is_some(),
            "simulation.max_repair_attempts",
        ),
        (
            "max_workspace_turns",
            controls.max_workspace_turns.is_some(),
            "simulation.max_workspace_turns",
        ),
        (
            "workspace_max_read_lines",
            controls.workspace_max_read_lines.is_some(),
            "simulation.workspace.max_read_lines",
        ),
        (
            "workspace_max_grep_matches",
            controls.workspace_max_grep_matches.is_some(),
            "simulation.workspace.max_grep_matches",
        ),
        (
            "workspace_max_line_len",
            controls.workspace_max_line_len.is_some(),
            "simulation.workspace.max_line_len",
        ),
        (
            "workspace_max_output_bytes",
            controls.workspace_max_output_bytes.is_some(),
            "simulation.workspace.max_output_bytes",
        ),
    ] {
        if supplied {
            problems.push(format!(
                "conversation_controls.{name} is simulator-owned and no longer accepted here: \
                 the environment is part of the scenario, so set it at \
                 {guidance} on the scenario definition (PATCH /api/scenarios/{{id}}, or \
                 POST /api/scenarios/{{id}}/fork to change a pinned one)"
            ));
        }
    }
    if controls
        .put_temperature
        .is_some_and(|value| !value.is_finite() || value < 0.0)
    {
        problems
            .push("conversation_controls.put_temperature must be finite and non-negative".into());
    }
    if controls.put_max_tokens == Some(0) {
        problems.push("conversation_controls.put_max_tokens must be greater than zero".into());
    }
    (!problems.is_empty()).then(|| problems.join("; "))
}

/// Migration guidance for request fields that moved to the scenario.
fn migrated_request_field(req: &InvestigateRequest) -> Option<String> {
    let mut problems = Vec::new();
    if req.scenario.is_some() || req.scenarios.is_some() {
        problems.push(
            "`scenario`/`scenarios` are no longer accepted inline: register the world once \
             with POST /api/scenarios (which is also where the initial workspace, the tool \
             surface and any Lua implementations belong), then submit investigations with \
             `scenario_id`"
                .to_string(),
        );
    }
    if req.sim_model.is_some() {
        problems.push(
            "`sim_model` moved to the scenario: set simulation.sim_model on the scenario \
             (PATCH /api/scenarios/{id}, or fork a pinned one). The simulator is the test \
             environment, so its model is part of the test case"
                .to_string(),
        );
    }
    if req.sim_thinking_level.is_some() {
        problems.push(
            "`sim_thinking_level` moved to the scenario: set \
             simulation.sim_thinking_level on the scenario"
                .to_string(),
        );
    }
    if !req.put.tools.is_empty() {
        problems.push(
            "`put.tools` is no longer authored per investigation: the tool surface belongs to \
             the scenario (definition.tools). Remove `tools` from the PUT here; the referenced \
             scenario's contracts are used"
                .to_string(),
        );
    }
    (!problems.is_empty()).then(|| problems.join("; "))
}

/// Create a job for `req`, spawn its run, and return the job id.
/// `workspace_seed` seeds the simulator's in-memory workspace for every
/// trace (cloned per trace; the seed is shared by Arc).
/// Resolve the PUT's own controls (investigation-owned) and the simulator's
/// (scenario-owned, already resolved into the runtime).
fn resolved_conversation_controls(
    controls: &ConversationControls,
    runtime: &prompt_explore::simulate::ScenarioRuntime,
    workspace: &WorkspaceToolLimits,
) -> (RunnerOptions, ResolvedConversationControls) {
    let mut runner = RunnerOptions::default();
    runner.put_temperature = controls.put_temperature.or(runner.put_temperature);
    runner.put_max_tokens = controls.put_max_tokens.or(runner.put_max_tokens);
    let resolved = ResolvedConversationControls {
        put_temperature: runner.put_temperature,
        put_max_tokens: runner.put_max_tokens,
        simulator: ResolvedSimulatorSettings {
            sim_temperature: runtime.simulator.temperature,
            sim_max_tokens: runtime.simulator.max_tokens,
            sim_max_repair_attempts: runtime.simulator.max_repair_attempts,
            max_workspace_turns: runtime.simulator.max_workspace_turns,
            lua: runtime.simulator.lua_simulation.clone(),
            workspace_max_read_lines: workspace.max_read_lines,
            workspace_max_grep_matches: workspace.max_grep_matches,
            workspace_max_line_len: workspace.max_line_len,
            workspace_max_output_bytes: workspace.max_output_bytes,
        },
    };
    (runner, resolved)
}

/// Workspace bounds for a scenario: its explicit limits, otherwise the
/// documented defaults (or the `PROMPT_EXPLORE_MAX_WORKSPACE_TURNS`-style env
/// override where one exists).
fn scenario_workspace_limits(
    settings: &prompt_explore::model::scenario::SimulationSettings,
) -> prompt_explore::simulate::WorkspaceToolLimits {
    let defaults = WorkspaceToolLimits::default();
    let limits = &settings.workspace;
    WorkspaceToolLimits {
        max_read_lines: limits.max_read_lines.unwrap_or(defaults.max_read_lines),
        max_grep_matches: limits.max_grep_matches.unwrap_or(defaults.max_grep_matches),
        max_line_len: limits.max_line_len.unwrap_or(defaults.max_line_len),
        max_output_bytes: limits.max_output_bytes.unwrap_or(defaults.max_output_bytes),
    }
}

/// Token usage and estimated cost for one job, from its frozen result when the
/// run is terminal and from its live trackers while it is still running. Cost is
/// attached only where the model catalog prices the role's model, so an absent
/// number means "we do not know this provider's price", never zero.
fn job_usage(job: &Job, pricing: &prompt_explore::llm::PricingMap) -> Option<UsageByRole> {
    if let Some(result) = &job.result {
        return Some(result.usage);
    }
    let (put_tracker, sim_tracker) = (job.put_tracker.as_ref()?, job.sim_tracker.as_ref()?);
    let mut put = put_tracker.totals();
    let mut sim = sim_tracker.totals();
    put.cost_usd = pricing.get(&job.put_model).and_then(|p| {
        cost_usd(
            put.input_tokens,
            put.cache_read_tokens,
            put.output_tokens,
            p,
        )
    });
    sim.cost_usd = pricing.get(&job.sim_model).and_then(|p| {
        cost_usd(
            sim.input_tokens,
            sim.cache_read_tokens,
            sim.output_tokens,
            p,
        )
    });
    Some(UsageByRole { put, sim })
}

fn epoch_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[allow(clippy::too_many_arguments)]
fn spawn_investigation(
    state: Arc<AppState>,
    req: &InvestigateRequest,
    put: PromptUnderTest,
    scenario: Scenario,
    scenario_revision: u64,
    scenario_definition_hash: String,
    runtime: prompt_explore::simulate::ScenarioRuntime,
    mut store: Option<&mut ScenarioStore>,
) -> String {
    let id = Uuid::new_v4().to_string();
    let progress = Arc::new(std::sync::Mutex::new(RunProgress::default()));
    let started_at = epoch_millis();
    // Resolve the model names now (defaults applied) so they can be
    // surfaced on the job immediately — visible while the run is still
    // in flight, not only after it finishes.
    let (put_model_requested, _) = resolved_models(req);
    let put_model =
        prompt_explore::llm::qualify_model(&put_model_requested, &state.default_provider);
    // The simulator's model is the SCENARIO's, resolved independently of the
    // PUT's: the environment is part of the test case.
    let sim_model = prompt_explore::llm::qualify_model(
        runtime
            .settings
            .sim_model
            .clone()
            .unwrap_or_else(|| MODEL.to_string())
            .as_str(),
        &state.default_provider,
    );
    let put_thinking_level = req.put_thinking_level;
    let sim_thinking_level = runtime.settings.sim_thinking_level;
    let workspace_limits = scenario_workspace_limits(&runtime.settings);
    let (runner_options, conversation_controls) =
        resolved_conversation_controls(&req.conversation_controls, &runtime, &workspace_limits);
    let workspace_files = runtime.workspace_seed.file_count();
    let workspace_hash = attributes::workspace_hash(&runtime.workspace_seed);
    let attributes = attributes::with_execution_attributes(
        system_attributes(
            &put_model,
            &sim_model,
            put_thinking_level,
            sim_thinking_level,
            &put,
            &workspace_hash,
            req.attributes.clone(),
        ),
        runtime.lua_enabled(),
        &req.investigation.budget,
    );
    let mut attributes = attributes;
    attributes.insert("scenario_id".into(), req.scenario_id.clone());
    attributes.insert("scenario_revision".into(), scenario_revision.to_string());
    attributes.insert("scenario_hash".into(), scenario_definition_hash.clone());

    // One tracker per role so usage is attributable to the PUT model vs. the
    // simulator model separately. The trackers live on the job as well as in the
    // spawned task, so a read can report spend at any point in the run.
    let trackers = state.client.clone().map(|inner| {
        (
            Arc::new(UsageTracker::new(inner.clone())),
            Arc::new(UsageTracker::new(inner)),
        )
    });
    state.jobs.lock().unwrap().insert(
        id.clone(),
        Job {
            status: JobStatus::Running,
            result: None,
            progress: progress.clone(),
            put_tracker: trackers.as_ref().map(|(put, _)| put.clone()),
            sim_tracker: trackers.as_ref().map(|(_, sim)| sim.clone()),
            started_at,
            finished_at: None,
            budget: req.investigation.budget.clone(),
            assessment: None,
            reason: req.investigation.reason.clone(),
            put: put.clone(),
            scenario: scenario.clone(),
            scenario_id: req.scenario_id.clone(),
            scenario_revision,
            scenario_definition_hash: scenario_definition_hash.clone(),
            put_model: put_model.clone(),
            sim_model: sim_model.clone(),
            put_thinking_level,
            sim_thinking_level,
            conversation_controls,
            workspace_files,
            grades: BTreeMap::new(),
            attributes,
        },
    );
    // Pin the scenario in the SAME critical section as the job insert.
    if let Some(store) = store.as_mut() {
        if let Err(error) = store.attach_investigation(&req.scenario_id, &id) {
            // The scenario disappeared between the lookup and here, which the
            // store lock makes impossible; fail loudly rather than run an
            // unpinned investigation.
            eprintln!(
                "could not pin scenario {} for investigation {id}: {error}",
                req.scenario_id
            );
        }
    }

    let state2 = state.clone();
    let id2 = id.clone();
    let resolved_inputs = req.resolved_inputs.clone();
    let investigation = req.investigation.clone();
    tokio::spawn(async move {
        // A server without provider credentials still accepts the submission
        // (there is no readiness probe and no invented verdict), then reports
        // the failure on the job instead of leaving it running forever.
        let Some((put_tracker, sim_tracker)) = trackers else {
            let mut jobs = state2.jobs.lock().unwrap();
            if let Some(job) = jobs.get_mut(&id2) {
                job.finished_at = Some(epoch_millis());
                job.status = JobStatus::Failed;
                job.result = Some(InvestigateResponse {
                    trace: None,
                    failure: Some(RunFailure {
                        stage: "provider".into(),
                        error: "no LLM client is configured on this server (no provider API \
                                key): the run never started. Configure a provider, or develop the \
                                simulation with POST /api/scenarios/{id}/simulations first"
                            .into(),
                    }),
                    usage: UsageByRole {
                        put: UsageTotals::default(),
                        sim: UsageTotals::default(),
                    },
                });
            }
            drop(jobs);
            state2.scenarios.lock().unwrap().finish_investigation(&id2);
            return;
        };
        let put_model_cost = put_model.clone();
        let sim_model_cost = sim_model.clone();
        let investigator = Investigator {
            runner_put: LlmRole {
                client: put_tracker.clone(),
                model: put_model.clone(),
                thinking_level: put_thinking_level,
            },
            runner_sim: LlmRole {
                client: sim_tracker.clone(),
                model: sim_model,
                thinking_level: sim_thinking_level,
            },
            runner_options,
        };

        let outcome = investigator
            .investigate(
                &investigation,
                &put,
                &runtime,
                resolved_inputs.as_ref(),
                Some(progress.clone()),
            )
            .await;

        let finished_at = epoch_millis();
        let trace = outcome.trace.as_ref().map(|trace| TraceView {
            execution: trace.execution.clone(),
            turns: trace.turns.clone(),
            final_world_state: trace.final_world_state.clone(),
            tool_calls: trace.tool_call_count(),
            resolved_inputs: trace.resolved_inputs.clone(),
            implementations: trace.implementations.clone(),
        });

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
            job.finished_at = Some(finished_at);
            job.status = if trace.is_some() {
                JobStatus::Done
            } else {
                JobStatus::Failed
            };
            job.result = Some(InvestigateResponse {
                trace,
                failure: outcome.failure,
                usage,
            });
        }
        drop(jobs);
        // A finished investigation still pins the scenario it ran; only the
        // "running" flag changes, so deletion stops being blocked by liveness
        // while the reference (and thus the lock) remains.
        state2.scenarios.lock().unwrap().finish_investigation(&id2);
    });

    id
}

#[derive(Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
struct ListInvestigationsQuery {
    /// Optional URL-encoded JSON object of exact stored attribute matches.
    attributes: Option<String>,
}

fn parse_attribute_filters(raw: &str) -> Result<BTreeMap<String, String>, String> {
    let filters: BTreeMap<String, String> = serde_json::from_str(raw).map_err(|error| {
        format!("attributes query must be a JSON object of string key/value pairs: {error}")
    })?;
    for (key, value) in &filters {
        if !attributes::valid_attribute_name(key) {
            return Err(format!(
                "attributes query key '{key}' fails ^[a-z][a-z0-9_]{{0,63}}$"
            ));
        }
        if value.len() > attributes::MAX_ATTRIBUTE_VALUE_BYTES {
            return Err(format!(
                "attributes query value for '{key}' exceeds {} UTF-8 bytes",
                attributes::MAX_ATTRIBUTE_VALUE_BYTES
            ));
        }
    }
    Ok(filters)
}

/// List all jobs (for the dashboard). Running jobs first, then by recency.
/// Returns summaries only — poll a job's id for full progress. Optionally pass
/// `attributes` as a URL-encoded JSON object of string key/value pairs; every
/// pair must exactly match a stored attribute (AND semantics). Omit it to list
/// all jobs. This filters only this listing, never POST /api/frontier.
#[utoipa::path(
    get,
    path = "/api/investigations",
    params(("attributes" = Option<String>, Query, description = "Optional URL-encoded JSON object of exact attribute string matches, e.g. `?attributes=%7B%22campaign%22%3A%22spring%22%7D`. Pairs are ANDed; omit to list all. Attribute keys use `^[a-z][a-z0-9_]{0,63}$`; malformed JSON/shapes/keys return 400. This does NOT filter POST /api/frontier.")),
    security(("api_token" = [])),
    responses(
        (status = 200, description = "All matching job summaries", body = [JobSummary]),
        (status = 400, description = "Malformed attributes query JSON, shape, or key"),
        (status = 401, description = "Missing or invalid bearer token")
    )
)]
async fn list_investigations(
    State(state): State<Arc<AppState>>,
    Query(query): Query<ListInvestigationsQuery>,
) -> Response {
    let filters = match query.attributes {
        None => BTreeMap::new(),
        Some(raw) => match parse_attribute_filters(&raw) {
            Ok(filters) => filters,
            Err(error) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({ "error": error })),
                )
                    .into_response();
            }
        },
    };
    // Pricing needs the model catalog (an await), so resolve it BEFORE taking
    // the job lock; the guard must never be held across an await.
    let pricing = catalog_pricing_map(&models_cached(&state).await.providers);
    let jobs = state.jobs.lock().unwrap();
    let mut rows: Vec<JobSummary> = jobs
        .iter()
        .filter(|(_, job)| {
            filters
                .iter()
                .all(|(key, value)| job.attributes.get(key) == Some(value))
        })
        .map(|(id, j)| {
            let progress = j.progress.lock().unwrap().snapshot();
            JobSummary {
                id: id.clone(),
                status: j.status,
                phase: progress.phase,
                execution: progress.execution,
                finished_at: j.finished_at,
                started_at: j.started_at,
                attributes: j.attributes.clone(),
                usage: job_usage(j, &pricing),
            }
        })
        .collect();
    // Running first, then newest-started first.
    rows.sort_by(|a, b| {
        let ar = a.status == JobStatus::Running;
        let br = b.status == JobStatus::Running;
        br.cmp(&ar).then_with(|| b.started_at.cmp(&a.started_at))
    });
    Json(rows).into_response()
}

/// Poll an investigation job. `progress` is always present (live model turns
/// while running, frozen on completion); `result` is present once the job is
/// `done` (trace, possibly budget-capped) or `failed` (failure evidence).
/// Prefer GET /api/investigations/{id}/evidence for reading/judging: it retains
/// actual tool responses and provenance without duplicating terminal progress.
/// Check execution.stop_reason, not status or nonempty text, for how the run stopped.
/// For an optimization loop, terminal status is the start of evaluation, not the
/// signal to edit the prompt: read /evidence, immediately PATCH grades/assessment
/// under the user's rubric, confirm the response, then read POST /api/frontier.
#[utoipa::path(
    get,
    path = "/api/investigations/{id}",
    params(("id" = String, Path, description = "Job id returned by POST /api/investigations")),
    security(("api_token" = [])),
    responses(
        (status = 200, description = "Job status + live progress (+ result when done or failed)", body = JobView),
        (status = 404, description = "Unknown job id"),
        (status = 401, description = "Missing or invalid bearer token")
    )
)]
async fn get_investigation(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<JobView>, StatusCode> {
    // Pricing needs the model catalog (an await); fetch it before either lock.
    let pricing = catalog_pricing_map(&models_cached(&state).await.providers);
    let jobs = state.jobs.lock().unwrap();
    let job = jobs.get(&id).ok_or(StatusCode::NOT_FOUND)?;
    // Take the progress lock ONCE: std Mutex is not reentrant, so two
    // `progress.lock()` calls in the same expression-building block
    // (phase, then clone) can deadlock the whole runtime if the first
    // temporary guard outlives the second lock(). Snapshot once.
    let mut progress_snapshot = job.progress.lock().unwrap().snapshot();
    progress_snapshot.usage = job_usage(job, &pricing);
    let phase = progress_snapshot.phase;
    Ok(Json(JobView {
        id: id.clone(),
        status: job.status,
        phase,
        started_at: job.started_at,
        finished_at: job.finished_at,
        budget: job.budget.clone(),
        assessment: job.assessment.clone(),
        reason: job.reason.clone(),
        put_model: job.put_model.clone(),
        sim_model: job.sim_model.clone(),
        put_thinking_level: job.put_thinking_level,
        sim_thinking_level: job.sim_thinking_level,
        conversation_controls: job.conversation_controls.clone(),
        workspace_files: job.workspace_files,
        scenario_id: job.scenario_id.clone(),
        scenario_revision: job.scenario_revision,
        scenario_definition_hash: job.scenario_definition_hash.clone(),
        grades: job.grades.clone(),
        attributes: job.attributes.clone(),
        put: job.put.clone(),
        scenario: job.scenario.clone(),
        progress: progress_snapshot,
        result: job.result.clone(),
    }))
}

/// Record caller judgment NOW, before the next prompt edit or candidate submission.
/// In a prompt-optimization loop, reading a trace or computing a local score does not
/// complete the evaluation: SEND this PATCH, confirm the successful response echoes
/// the annotations, then POST /api/frontier and inspect its points before revising.
/// A script containing a PATCH is not a recorded grade until it executes successfully.
///
/// Missing requested quality grades exclude a run from that frontier's coordinates,
/// not from its membership/backlog. Measured-only frontiers need no grades. Grading is
/// how the user's quality rubric becomes visible alongside cost, not a server verdict.
///
/// `grades` is caller-owned numeric judgment, one number per axis you and your user
/// agreed on (for example `found_all: 0.5`, `precision: 1.0`, `tone_of_voice: 0.8`);
/// `attributes` is caller-owned string metadata (for example `label: "baseline"`, or a
/// prompt-version tag you group by later). The harness records both and never
/// interprets a grade. Grade the run against the EVIDENCE — the tool responses it
/// actually received, not the plausibility of its final answer. `assessment` is where
/// you say why: the rubric scale and the turn/exchange the judgment rests on.
///
/// Both maps have merge semantics: a number/string sets or overwrites and
/// JSON `null` deletes that key. `assessment` is caller-owned summary, rubric and
/// zero-based evidence references: object replaces the whole assessment, null
/// clears it, absent leaves it unchanged. All fields validate before ANY apply.
/// The response echoes FULL updated grades, attributes and assessment.
/// A numeric fidelity grade needs review of ACTUAL tool responses, not merely
/// workspace_ops or computed counts. If simulation is inadequate, record that in
/// assessment and withhold unjustified grades; this is not a harness verdict.
/// Grade names and attribute names use `^[a-z][a-z0-9_]{0,63}$`; grade names cannot
/// be measured axes. The literal measured names are `put_input_tokens`,
/// `put_output_tokens`, `put_cache_read_tokens`, `put_cost_usd`,
/// `sim_input_tokens`, `sim_output_tokens`, `sim_cache_read_tokens`,
/// `sim_cost_usd`, `steps_per_trace_avg`, `steps_per_trace_min`,
/// `steps_per_trace_max`, `steps_per_trace_stdev`, `elapsed_ms`,
/// `resolving_inputs_ms` and `put_loop_ms`.
///
/// PATCH is allowed while a job runs. POST /api/frontier always considers ALL
/// current jobs: running, failed, ungraded, or unavailable members appear as
/// explicit exclusions/backlog in a successful grouped response. A group has
/// null coordinates until it has at least one common complete cohort; poll
/// and PATCH missing grades, then submit the same frontier request again.
#[utoipa::path(
    patch,
    path = "/api/investigations/{id}",
    params(("id" = String, Path, description = "Job id returned by POST /api/investigations")),
    request_body(content = InvestigationPatch, description = "Optional grades/attributes maps and/or assessment (tags is not an alias). Maps merge; null deletes a map key. Assessment object replaces, null clears, absent preserves. All validate atomically, including existing turn/exchange references and a 65536-byte total assessment text cap. Response echoes all three. Read /evidence before judging and save your rubric/limitations, not only a number."),
    security(("api_token" = [])),
    responses(
        (status = 200, description = "Updated full grades, attributes and assessment", body = InvestigationPatchView),
        (status = 400, description = "Invalid grades or attributes. Attribute keys use `^[a-z][a-z0-9_]{0,63}$`, values are strings ≤1024 bytes, and immutable provenance keys (`put_model`, `sim_model`, `put_thinking`, `sim_thinking`, `prompt_hash`, `workspace_hash`, `simulation_backend`, `step_budget`, `token_budget`) cannot change."),
        (status = 401, description = "Missing or invalid bearer token"),
        (status = 404, description = "Unknown job id")
    )
)]
async fn patch_investigation(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    body: Result<Json<InvestigationPatch>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let patch = match body {
        Ok(Json(p)) => p,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": format!("body is not valid investigation patch JSON: {e}")
                })),
            )
                .into_response();
        }
    };
    if patch.grades.is_none() && patch.attributes.is_none() && patch.assessment.is_none() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "patch must contain grades, attributes and/or assessment"})),
        )
            .into_response();
    }
    // Validate every supplied map before taking the job lock or mutating it:
    // a bad attribute cannot leave its otherwise-valid grade sibling half-applied.
    if let Some(grades) = &patch.grades {
        if let Err(problems) = frontier::validate_grades_patch(&GradesPatch {
            grades: grades.clone(),
        }) {
            return (StatusCode::BAD_REQUEST, Json(problems)).into_response();
        }
    }
    if let Some(attributes) = &patch.attributes {
        if let Err(error) = validate_attribute_patch(attributes) {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": error })),
            )
                .into_response();
        }
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
    if let Some(Some(assessment)) = &patch.assessment {
        let snapshot = job.progress.lock().unwrap().snapshot();
        let turns = job
            .result
            .as_ref()
            .and_then(|r| r.trace.as_ref())
            .map(|t| t.turns.as_slice())
            .unwrap_or(&snapshot.turns);
        if let Err(error) = assessment.validate(turns) {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": error})),
            )
                .into_response();
        }
    }
    if let Some(assessment) = patch.assessment {
        job.assessment = assessment;
    }
    if let Some(grades) = patch.grades {
        for (axis, value) in grades {
            match value {
                Some(v) => {
                    job.grades.insert(axis, v);
                }
                None => {
                    job.grades.remove(&axis);
                }
            }
        }
    }
    if let Some(attributes) = patch.attributes {
        for (key, value) in attributes {
            match value {
                Some(value) => {
                    job.attributes.insert(key, value);
                }
                None => {
                    job.attributes.remove(&key);
                }
            }
        }
    }
    (
        StatusCode::OK,
        Json(InvestigationPatchView {
            grades: job.grades.clone(),
            attributes: job.attributes.clone(),
            assessment: job.assessment.clone(),
        }),
    )
        .into_response()
}

/// Delete an investigation: remove the job — its traces, grades, attributes, and
/// progress — from the server's memory. Irreversible: the evidence is gone
/// (a re-run means POSTing a new investigation). Useful for pruning a
/// campaign: the next grouped POST /api/frontier considers all REMAINING jobs
/// and no longer includes this member. RUNNING jobs cannot be deleted (409): a run cannot be
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
            drop(jobs);
            // Forgetting the investigation unlocks the scenario it pinned as
            // soon as it held the last reference.
            state
                .scenarios
                .lock()
                .unwrap()
                .detach_investigation(&id);
            (
                StatusCode::OK,
                Json(serde_json::json!({ "deleted": id })),
            )
                .into_response()
        }
    }
}

/// Assemble the harness-side facts the grouped frontier needs from one job.
/// Thin data plumbing: all grouping, exclusion, and dominance arithmetic lives
/// in core::frontier::grouped.
fn snapshot_of(id: &str, job: &Job) -> InvestigationSnapshot {
    let result = job.result.as_ref();
    InvestigationSnapshot {
        id: id.to_string(),
        status: match job.status {
            JobStatus::Running => SnapshotStatus::Running,
            // A done job has one completed trace. Failed evidence is retained
            // on the job result but never becomes a cheap frontier candidate.
            JobStatus::Done if result.is_none_or(|r| r.trace.is_none()) => SnapshotStatus::Failed,
            JobStatus::Done => SnapshotStatus::Done,
            JobStatus::Failed => SnapshotStatus::Failed,
        },
        put_id: Some(job.put.id.clone()).filter(|p| !p.is_empty()),
        grades: job.grades.clone(),
        usage: result.map(|r| r.usage),
        timing: result
            .and_then(|r| r.trace.as_ref())
            .map(|t| t.execution.timing.clone()),
        put_model: Some(job.put_model.clone()),
        sim_model: Some(job.sim_model.clone()),
        // One job has one trace, so its snapshot contributes one step count.
        // A tool exchange is one step; a text-only completion is one step.
        steps_per_trace: result
            .and_then(|r| r.trace.as_ref())
            .map(|trace| {
                vec![
                    trace
                        .turns
                        .iter()
                        .map(|turn| turn.tool_exchanges.len().max(1) as u64)
                        .sum(),
                ]
            })
            .unwrap_or_default(),
    }
}

#[derive(Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
struct FrontierQuery {
    /// `json` (default): grouped points and preliminary/exclusion evidence.
    /// `svg`: exactly two axes rendered as a grouped scatter plot.
    #[serde(default)]
    format: Option<String>,
}

/// Read the comparison AFTER recording judgments and BEFORE changing the prompt.
/// SEND this request and inspect the returned `points`; writing a local comparison
/// table or saying you will check the frontier later does not perform this step.
/// Use the quality axes agreed with the user alongside measured cost/effort. Report
/// the groups' values, membership and exclusions to the user before recommending a
/// candidate; non-dominance alone is not proof of improvement or generalization.
///
/// There is no investigation-selection list: every investigation the server holds is
/// a candidate, and a card filter never limits candidacy. `group_by` chooses the
/// provenance/campaign attributes that define one candidate point (default: put
/// model, thinking setting, and behavior-only prompt hash), so two runs share a group
/// exactly when their selected attribute values match; other properties may differ.
/// Coordinates average requested axes over the SAME completed cohort having EVERY
/// requested value. Measured axes (tokens, cost, steps, durations) need no caller
/// grades; judged axes are the caller's PATCHed grades. An ungraded member remains
/// visible in the group, and is excluded only when a requested grade is missing.
///
/// `points[].values = null` means NO member has the full requested set of values.
/// Read `excluded` to distinguish awaiting grades from running/failed jobs and
/// unavailable measured axes. PATCH justified missing grades (or clear unjustified
/// ones and record why in assessment), then request the frontier again. Preliminary
/// groups have exclusions but still participate in dominance when values exist;
/// report those limitations rather than treating a provisional comparison as settled. A failed run (`result.failure` present, `result.trace` null) is a
/// failed exclusion; a done job contributes its single trace. Judging trace adequacy
/// remains the caller's responsibility, and the harness never substitutes a verdict
/// of its own.
#[utoipa::path(
    post,
    path = "/api/frontier",
    params(("format" = Option<String>, Query, description = "`json` (default) or `svg`")),
    request_body(content = GroupedFrontierRequest, description = "Grouping attribute keys (default [`put_model`,`put_thinking`,`prompt_hash`]) and Pareto axes. The server considers every current job; do NOT send the legacy `investigations` selection field (unknown fields are rejected). `label` is an editable UI display attribute; system provenance attributes describe resolved provider-qualified models/settings and canonical prompt/workspace SHA-256 hashes."),
    security(("api_token" = [])),
    responses(
        (status = 200, description = "Grouped frontier and exclusion evidence, including running/failed/awaiting-grades/unavailable members. Pending groups have null coordinates; poll investigations and resubmit after they finish or receive grades.", body = GroupedFrontierResponse),
        (status = 400, description = "Malformed body or unknown ?format"),
        (status = 401, description = "Missing or invalid bearer token"),
        (status = 422, description = "Invalid grouping/axis request (for example bad attribute or axis name, duplicate axis, incompatible direction, or SVG arity).", body = frontier::FrontierError)
    )
)]
async fn frontier(
    State(state): State<Arc<AppState>>,
    Query(q): Query<FrontierQuery>,
    body: Result<Json<GroupedFrontierRequest>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let format =
        match q.format.as_deref() {
            None | Some("json") => FrontierFormat::Json,
            Some("svg") => FrontierFormat::Svg,
            Some(other) => return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": format!("unknown format '{other}' — use ?format=json or ?format=svg")
                })),
            )
                .into_response(),
        };
    let req = match body {
        Ok(Json(r)) => r,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": format!("body is not valid grouped frontier request JSON: {e}")
                })),
            )
                .into_response();
        }
    };
    // Grouped compute intentionally sees ALL jobs, including live ones, so
    // the response explains campaign backlog instead of omitting it.
    let snapshots: BTreeMap<String, GroupedSnapshot> = {
        let jobs = state.jobs.lock().unwrap();
        jobs.iter()
            .map(|(id, job)| {
                (
                    id.clone(),
                    GroupedSnapshot {
                        snapshot: snapshot_of(id, job),
                        attributes: job.attributes.clone(),
                    },
                )
            })
            .collect()
    };
    match grouped::compute_grouped(&req, &snapshots, format) {
        Err(error) => (StatusCode::UNPROCESSABLE_ENTITY, Json(error)).into_response(),
        Ok(response) => match format {
            FrontierFormat::Json => (StatusCode::OK, Json(response)).into_response(),
            FrontierFormat::Svg => {
                let svg = frontier::render_grouped(
                    &response,
                    &frontier::svg::PlotAxis::new(&req.axes[0].name, req.axes[0].better),
                    &frontier::svg::PlotAxis::new(&req.axes[1].name, req.axes[1].better),
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
const LUA_API_MD: &str = include_str!("../../docs/lua-api.md");
const VENDOR_PREACT: &str = include_str!("../static/vendor/preact.mjs");
const VENDOR_HOOKS: &str = include_str!("../static/vendor/hooks.mjs");
const VENDOR_HTM: &str = include_str!("../static/vendor/htm.mjs");

async fn vendor_preact() -> impl axum::response::IntoResponse {
    (
        [
            ("content-type", "text/javascript;charset=utf-8"),
            ("cache-control", "no-cache"),
        ],
        VENDOR_PREACT,
    )
}
async fn vendor_hooks() -> impl axum::response::IntoResponse {
    (
        [
            ("content-type", "text/javascript;charset=utf-8"),
            ("cache-control", "no-cache"),
        ],
        VENDOR_HOOKS,
    )
}
async fn vendor_htm() -> impl axum::response::IntoResponse {
    (
        [
            ("content-type", "text/javascript;charset=utf-8"),
            ("cache-control", "no-cache"),
        ],
        VENDOR_HTM,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::INDEX_HTML;
    use axum::http::Request as HttpRequest;
    use prompt_explore::model::Budget;
    use prompt_explore::simulate::unpack_zip_with_limits;
    use std::io::Write;
    use tower::ServiceExt; // oneshot against the REAL router

    /// A state with no LLM client: enough for the grading/frontier
    /// surface (which is LLM-independent by design).
    fn test_state() -> Arc<AppState> {
        Arc::new(AppState {
            client: None,
            jobs: Mutex::new(HashMap::new()),
            scenarios: Mutex::new(ScenarioStore::new()),
            probes: Mutex::new(HashMap::new()),
            default_provider: "zai".into(),
            models_client: prompt_explore::llm::GenaiClient::builder().build().unwrap(),
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

    /// Seed a DONE job with one trace of `steps` completions. A fixture job
    /// never fabricates multiple conversations.
    fn seed_done_job(state: &Arc<AppState>, id: &str, put_id: &str, out: u64, steps: usize) {
        let (id, job) = fabricate_done_job(id, put_id, "demo template", out, steps);
        state.jobs.lock().unwrap().insert(id, job);
    }

    fn zip_bytes(entries: &[(&str, &[u8])], modified: zip::DateTime) -> Vec<u8> {
        let mut bytes = Vec::new();
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(&mut bytes));
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored)
            .last_modified_time(modified);
        for (path, content) in entries {
            writer.start_file(*path, options).unwrap();
            writer.write_all(content).unwrap();
        }
        writer.finish().unwrap();
        bytes
    }

    async fn patch_job(app: &Router, id: &str, body: &str) -> (StatusCode, serde_json::Value) {
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

    /// Register a scenario through the real HTTP route, with an optional
    /// workspace archive as the multipart `workspace` part.
    async fn create_scenario_multipart(app: &Router, archive: Vec<u8>) -> serde_json::Value {
        let boundary = "workspace-attribute-test";
        let request = serde_json::json!({
            "scenario": {
                "world": "Fixture world.",
                "tools": [{
                    "name": "lookup",
                    "description": "Look something up.",
                    "parameters": {"type": "object"},
                    "side_effect": "read"
                }]
            }
        })
        .to_string();
        let mut body = Vec::new();
        body.extend_from_slice(format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"request\"\r\nContent-Type: application/json\r\n\r\n{request}\r\n--{boundary}\r\nContent-Disposition: form-data; name=\"workspace\"; filename=\"workspace.zip\"\r\nContent-Type: application/zip\r\n\r\n"
        ).as_bytes());
        body.extend_from_slice(&archive);
        body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
        let response = app
            .clone()
            .oneshot(
                HttpRequest::post("/api/scenarios")
                    .header(
                        "content-type",
                        format!("multipart/form-data; boundary={boundary}"),
                    )
                    .body(axum::body::Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
            .await
            .unwrap();
        assert_eq!(
            status,
            StatusCode::CREATED,
            "{}",
            String::from_utf8_lossy(&bytes)
        );
        serde_json::from_slice(&bytes).unwrap()
    }

    /// Register a minimal scenario directly, for tests about investigations.
    fn register_scenario(state: &Arc<AppState>) -> String {
        state
            .scenarios
            .lock()
            .unwrap()
            .create(
                definition_with_tools(vec![scenario_tool("lookup", None)]),
                Workspace::empty(),
                None,
                1,
            )
            .unwrap()
    }

    fn investigation_body(state: &Arc<AppState>) -> serde_json::Value {
        serde_json::json!({
            "scenario_id": register_scenario(state),
            "investigation": {"budget": {"max_steps_per_trace": 1}},
            "put": {"id": "x", "template": "t", "design_goals": "g"}
        })
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

    async fn get_json(app: &Router, path: &str) -> (StatusCode, Value) {
        let response = app
            .clone()
            .oneshot(
                HttpRequest::get(path)
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let body = axum::body::to_bytes(response.into_body(), 1 << 20)
            .await
            .unwrap();
        (status, serde_json::from_slice(&body).unwrap_or(Value::Null))
    }

    #[tokio::test]
    async fn assessment_replace_clear_and_atomic_validation() {
        let state = test_state();
        seed_done_job(&state, "assess", "reviewer", 10, 1);
        let app = build_app(state);
        let (status, first) = patch_job(&app, "assess", r#"{"grades":{"quality":0.5},"assessment":{"summary":"Uncertain","rubric":"0..1","evidence":[{"turn":0,"note":"Final completion"}]}}"#).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(first["assessment"]["summary"], "Uncertain");
        let (status, _) = patch_job(&app, "assess", r#"{"grades":{"quality":1},"attributes":{"label":"not applied"},"assessment":{"summary":"invalid index","evidence":[{"turn":0,"exchange":0,"note":"missing"}]}}"#).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let (_, saved) = get_json(&app, "/api/investigations/assess").await;
        assert_eq!(saved["grades"]["quality"], 0.5);
        assert_ne!(saved["attributes"]["label"], "not applied");
        assert_eq!(saved["assessment"]["summary"], "Uncertain");
        let (_, echoed) = patch_job(&app, "assess", r#"{"attributes":{"label":"safe"}}"#).await;
        assert_eq!(echoed["assessment"], first["assessment"]);
        let (status, replaced) = patch_job(
            &app,
            "assess",
            r#"{"assessment":{"summary":"Revised interpretation"}}"#,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(replaced["assessment"]["evidence"], serde_json::json!([]));
        let (status, cleared) = patch_job(&app, "assess", r#"{"assessment":null}"#).await;
        assert_eq!(status, StatusCode::OK);
        assert!(cleared["assessment"].is_null());
        assert_eq!(cleared["grades"]["quality"], 0.5);
    }

    #[tokio::test]
    async fn evidence_preserves_actual_responses_without_duplicate_progress() {
        let state = test_state();
        seed_done_job(&state, "ev", "reviewer", 20, 1);
        let exchange = prompt_explore::model::simulation::ToolExchange {
            call: prompt_explore::model::simulation::ToolCall {
                name: "list_files".into(),
                args: serde_json::json!({"path":"."}),
            },
            response: serde_json::json!({"error":"invalid path"}),
            lua_execution: Some(prompt_explore::model::simulation::LuaExecutionRecord {
                tool: "list_files".into(),
                source_hash: "fixture-hash".into(),
                outcome: prompt_explore::model::simulation::LuaOutcome::Computed,
                detail: None,
                discarded_workspace_ops: vec![],
            }),
            sim_thinking: Some("supporting reasoning".into()),
            world_state_after: None,
            workspace_ops: vec![],
        };
        {
            let mut jobs = state.jobs.lock().unwrap();
            let job = jobs.get_mut("ev").unwrap();
            let trace = job.result.as_mut().unwrap().trace.as_mut().unwrap();
            trace.turns[0].tool_exchanges.push(exchange.clone());
            trace.execution.stop_reason =
                Some(prompt_explore::model::simulation::RunStopReason::StepBudget);
        }
        let app = build_app(state.clone());
        let (status, ev) = get_json(&app, "/api/investigations/ev/evidence").await;
        assert_eq!(status, StatusCode::OK);
        assert!(ev.get("progress").is_none() && ev.get("result").is_none());
        assert_eq!(
            ev["turns"][0]["tool_exchanges"][0]["response"]["error"],
            "invalid path"
        );
        assert_eq!(ev["execution"]["stop_reason"], "step_budget");
        assert_eq!(ev["budget"]["max_steps_per_trace"], 1);
        assert_eq!(ev["finished_at"], 0);
        assert!(ev["scenario"]["world"].is_string());
        // Failure path uses partial progress, never inventing a completed trace.
        {
            let mut jobs = state.jobs.lock().unwrap();
            let job = jobs.get_mut("ev").unwrap();
            let result = job.result.as_mut().unwrap();
            let trace = result.trace.take().unwrap();
            let mut progress = job.progress.lock().unwrap();
            progress.turns = trace.turns;
            progress.execution.unrendered_call =
                Some(prompt_explore::model::simulation::ToolCall {
                    name: "search_text".into(),
                    args: serde_json::json!({"pattern": "a|b"}),
                });
            progress.finish(prompt_explore::model::simulation::RunStopReason::RuntimeFailure);
            job.status = JobStatus::Failed;
            result.failure = Some(RunFailure {
                stage: "runner".into(),
                error: "provider failure".into(),
            });
        }
        let (_, failed) = get_json(&app, "/api/investigations/ev/evidence").await;
        assert_eq!(failed["execution"]["stop_reason"], "runtime_failure");
        assert_eq!(
            failed["turns"][0]["tool_exchanges"][0]["response"],
            serde_json::json!({"error":"invalid path"})
        );
        assert_eq!(failed["failure"]["error"], "provider failure");
        assert_eq!(
            failed["execution"]["unrendered_call"],
            serde_json::json!({"name":"search_text","args":{"pattern":"a|b"}}),
            "the failing request is retained rather than only implied by counters"
        );
        assert!(failed["final_world_state"].is_null());
        assert_eq!(
            get_json(&app, "/api/investigations/missing/evidence")
                .await
                .0,
            StatusCode::NOT_FOUND
        );
    }

    #[tokio::test]
    async fn evidence_retains_charged_but_unaccepted_token_cutoff_completion() {
        let state = test_state();
        seed_done_job(&state, "cap", "reviewer", 20, 0);
        {
            let mut jobs = state.jobs.lock().unwrap();
            let execution = &mut jobs
                .get_mut("cap")
                .unwrap()
                .result
                .as_mut()
                .unwrap()
                .trace
                .as_mut()
                .unwrap()
                .execution;
            execution.stop_reason =
                Some(prompt_explore::model::simulation::RunStopReason::TokenBudget);
            execution.budget_cutoff_completion =
                Some(prompt_explore::model::simulation::BudgetCutoffCompletion {
                    model_output: Some("received but not accepted".into()),
                    thinking: None,
                    tool_calls: vec![prompt_explore::llm::ToolCallRequest {
                        id: "raw-id".into(),
                        name: "write".into(),
                        arguments: "malformed raw arguments".into(),
                    }],
                });
        }
        let app = build_app(state);
        let (_, evidence) = get_json(&app, "/api/investigations/cap/evidence").await;
        assert_eq!(evidence["turns"], serde_json::json!([]));
        assert_eq!(evidence["execution"]["stop_reason"], "token_budget");
        assert_eq!(
            evidence["execution"]["budget_cutoff_completion"]["model_output"],
            "received but not accepted"
        );
        assert_eq!(
            evidence["execution"]["budget_cutoff_completion"]["tool_calls"][0]["arguments"],
            "malformed raw arguments"
        );
    }

    #[tokio::test]
    async fn evidence_requires_auth_like_other_api_routes() {
        let mut state = test_state();
        Arc::get_mut(&mut state).unwrap().api_token = Some(sha256(b"test-secret"));
        seed_done_job(&state, "secure", "reviewer", 20, 1);
        let app = build_app(state);
        assert_eq!(
            get_json(&app, "/api/investigations/secure/evidence")
                .await
                .0,
            StatusCode::UNAUTHORIZED
        );
        let response = app
            .oneshot(
                HttpRequest::get("/api/investigations/secure/evidence")
                    .header("authorization", "Bearer test-secret")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn patch_grades_merges_deletes_and_echoes() {
        let state = test_state();
        seed_done_job(&state, "job-1", "cancel-bot", 100, 2);
        let app = build_app(state);

        // Set two axes.
        let (code, v) = patch_job(
            &app,
            "job-1",
            r#"{"grades": {"tone_of_voice": 0.8, "clarity": 0.6}}"#,
        )
        .await;
        assert_eq!(code, StatusCode::OK);
        assert_eq!(v["grades"]["tone_of_voice"], 0.8);
        assert_eq!(v["grades"]["clarity"], 0.6);

        // Overwrite one, delete the other; echo shows the merged map.
        let (code, v) = patch_job(
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
        assert!(v["attributes"].is_object());
        assert!(v.get("tags").is_none());

        // The compact list shape uses the same vocabulary.
        let res = app
            .oneshot(
                HttpRequest::get("/api/investigations")
                    .body(String::new())
                    .unwrap(),
            )
            .await
            .unwrap();
        let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
            .await
            .unwrap();
        let summaries: Value = serde_json::from_slice(&bytes).unwrap();
        assert!(summaries[0]["attributes"].is_object());
        assert!(summaries[0].get("tags").is_none());
    }

    #[tokio::test]
    async fn job_view_has_singular_trace_progress_and_failure_shapes() {
        let state = test_state();
        seed_done_job(&state, "done", "cancel-bot", 100, 2);
        seed_done_job(&state, "failed", "cancel-bot", 100, 2);
        {
            let mut jobs = state.jobs.lock().unwrap();
            let failed = jobs.get_mut("failed").unwrap();
            failed.status = JobStatus::Failed;
            let result = failed.result.as_mut().unwrap();
            result.trace = None;
            result.failure = Some(RunFailure {
                stage: "runner".into(),
                error: "fixture failure".into(),
            });
        }
        let app = build_app(state);
        for (id, status) in [("done", "done"), ("failed", "failed")] {
            let response = app
                .clone()
                .oneshot(
                    HttpRequest::get(format!("/api/investigations/{id}"))
                        .body(String::new())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let body: Value = serde_json::from_slice(
                &axum::body::to_bytes(response.into_body(), 1 << 20)
                    .await
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(body["status"], status);
            assert!(body["scenario"].is_object());
            assert!(body.get("scenarios").is_none());
            assert!(body["progress"].get("scenarios").is_none());
            assert!(body["progress"]["turns"].is_array());
            assert!(body["result"]["usage"].is_object());
            assert!(body["result"].get("result").is_none());
            assert!(body["result"].get("attempts").is_none());
            assert!(body["result"].get("failures").is_none());
            if id == "done" {
                assert!(body["result"]["trace"].is_object());
                assert!(body["result"]["failure"].is_null());
            } else {
                assert!(body["result"]["trace"].is_null());
                assert_eq!(body["result"]["failure"]["stage"], "runner");
            }
        }
    }

    #[tokio::test]
    async fn list_investigations_filters_attributes_with_exact_and_semantics() {
        let state = test_state();
        seed_done_job(&state, "one", "cancel-bot", 100, 1);
        seed_done_job(&state, "two", "cancel-bot", 100, 1);
        seed_done_job(&state, "three", "cancel-bot", 100, 1);
        {
            let mut jobs = state.jobs.lock().unwrap();
            jobs.get_mut("one").unwrap().attributes.extend([
                ("campaign".into(), "spring".into()),
                ("variant".into(), "a".into()),
            ]);
            jobs.get_mut("two").unwrap().attributes.extend([
                ("campaign".into(), "spring".into()),
                ("variant".into(), "b".into()),
            ]);
            jobs.get_mut("three")
                .unwrap()
                .attributes
                .insert("campaign".into(), "fall".into());
        }
        let app = build_app(state);
        let get = |uri: &str| HttpRequest::get(uri).body(String::new()).unwrap();
        let all = app
            .clone()
            .oneshot(get("/api/investigations"))
            .await
            .unwrap();
        let all: Value = serde_json::from_slice(
            &axum::body::to_bytes(all.into_body(), 1 << 20)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(all.as_array().unwrap().len(), 3);
        assert!(all[0]["phase"].is_string());
        assert!(all[0].get("scenarios").is_none());
        // Spend is readable from the polling endpoint: a caller can total the
        // campaign without fetching every job's evidence.
        assert_eq!(
            all[0]["usage"]["put"]["input_tokens"],
            all[0]["attributes"].is_object().then_some(4200).unwrap()
        );
        assert!(all[0]["usage"]["sim"]["input_tokens"].is_number());

        let filtered = app
            .clone()
            .oneshot(get("/api/investigations?attributes=%7B%22campaign%22%3A%22spring%22%2C%22variant%22%3A%22a%22%7D"))
            .await
            .unwrap();
        let filtered: Value = serde_json::from_slice(
            &axum::body::to_bytes(filtered.into_body(), 1 << 20)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(filtered.as_array().unwrap().len(), 1);
        assert_eq!(filtered[0]["id"], "one");

        let malformed = app
            .clone()
            .oneshot(get("/api/investigations?attributes=%5B%5D"))
            .await
            .unwrap();
        assert_eq!(malformed.status(), StatusCode::BAD_REQUEST);
        let bad_key = app
            .oneshot(get(
                "/api/investigations?attributes=%7B%22bad%20key%22%3A%22x%22%7D",
            ))
            .await
            .unwrap();
        assert_eq!(bad_key.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn multipart_workspace_attributes_ignore_archive_metadata_but_track_content() {
        let early = zip::DateTime::from_date_and_time(2024, 1, 2, 3, 4, 6).unwrap();
        let late = zip::DateTime::from_date_and_time(2025, 2, 3, 4, 5, 8).unwrap();
        let first = zip_bytes(&[("a.txt", b"alpha"), ("dir/b.txt", b"beta")], early);
        let reordered = zip_bytes(&[("dir/b.txt", b"beta"), ("a.txt", b"alpha")], late);
        let changed = zip_bytes(&[("a.txt", b"ALPHA"), ("dir/b.txt", b"beta")], late);
        let app = build_app(test_state());
        // Registration computes the hash, so the identity is reproducible from
        // the response alone.
        let first = create_scenario_multipart(&app, first).await;
        let reordered = create_scenario_multipart(&app, reordered).await;
        let changed = create_scenario_multipart(&app, changed).await;
        assert_eq!(first["workspace_hash"], reordered["workspace_hash"]);
        assert_ne!(first["workspace_hash"], changed["workspace_hash"]);
        // Each registration is its own scenario with its own revision.
        assert_eq!(first["revision"], 1);
    }

    #[test]
    fn workspace_hash_ignores_zip_order_and_metadata_but_not_content() {
        let early = zip::DateTime::from_date_and_time(2024, 1, 2, 3, 4, 6).unwrap();
        let late = zip::DateTime::from_date_and_time(2025, 2, 3, 4, 5, 8).unwrap();
        let first = zip_bytes(&[("a.txt", b"alpha"), ("dir/b.txt", b"beta")], early);
        let reordered = zip_bytes(&[("dir/b.txt", b"beta"), ("a.txt", b"alpha")], late);
        let changed = zip_bytes(&[("a.txt", b"ALPHA"), ("dir/b.txt", b"beta")], late);
        let unpack = |archive: &[u8]| {
            unpack_zip_with_limits(
                archive,
                workspace_compressed_limit(),
                workspace_decompressed_limit(),
            )
            .unwrap()
        };
        assert_eq!(
            attributes::workspace_hash(&unpack(&first)),
            attributes::workspace_hash(&unpack(&reordered)),
            "archive ordering/timestamps are not workspace content"
        );
        assert_ne!(
            attributes::workspace_hash(&unpack(&first)),
            attributes::workspace_hash(&unpack(&changed)),
            "path content changes must be visible in provenance"
        );
    }

    #[tokio::test]
    async fn attributes_merge_delete_and_invalid_siblings_are_atomic() {
        let state = test_state();
        seed_done_job(&state, "job-1", "cancel-bot", 100, 2);
        let app = build_app(state.clone());

        let (code, view) = patch_job(
            &app,
            "job-1",
            r#"{"grades":{"clarity":0.6},"attributes":{"label":"baseline","campaign":"spring","empty":""}}"#,
        )
        .await;
        assert_eq!(code, StatusCode::OK);
        assert_eq!(view["grades"]["clarity"], 0.6);
        assert_eq!(view["attributes"]["label"], "baseline");
        assert_eq!(
            view["attributes"]["empty"], "",
            "empty string is a stored value, not deletion"
        );
        assert_eq!(
            view["attributes"]["prompt_hash"].as_str().unwrap().len(),
            64
        );
        assert_eq!(
            view["attributes"]["workspace_hash"].as_str().unwrap().len(),
            64
        );

        // Both maps validate before either changes: an immutable attribute cannot
        // smuggle through a grade update as a partial PATCH.
        let (code, _) = patch_job(
            &app,
            "job-1",
            r#"{"grades":{"clarity":0.9},"attributes":{"put_model":"forged"}}"#,
        )
        .await;
        assert_eq!(code, StatusCode::BAD_REQUEST);
        let job = state
            .jobs
            .lock()
            .unwrap()
            .get("job-1")
            .unwrap()
            .grades
            .clone();
        assert_eq!(job["clarity"], 0.6);

        // The feature was unreleased when renamed: do not silently retain the
        // misleading old vocabulary as a compatibility alias.
        let (code, _) = patch_job(&app, "job-1", r#"{"tags":{"label":"legacy"}}"#).await;
        assert_eq!(code, StatusCode::BAD_REQUEST);
        assert_eq!(
            state.jobs.lock().unwrap()["job-1"].attributes["label"],
            "baseline"
        );

        let (code, view) = patch_job(
            &app,
            "job-1",
            r#"{"grades":{"clarity":0.8},"attributes":{"label":null}}"#,
        )
        .await;
        assert_eq!(code, StatusCode::OK);
        assert_eq!(view["grades"]["clarity"], 0.8);
        assert!(view["attributes"].get("label").is_none());
        assert_eq!(view["attributes"]["campaign"], "spring");
        assert_eq!(view["attributes"]["empty"], "");
    }

    #[tokio::test]
    async fn post_rejects_immutable_attributes_before_launch() {
        let state = test_state();
        let app = build_app(state.clone());
        let body = serde_json::json!({
            "investigation": {"budget": {"max_steps_per_trace": 2}},
            "put": {"id": "x", "template": "t", "design_goals": "g", "tools": []},
            "scenario": {"world": "Fixture world."},
            "attributes": {"put_model": "forged"}
        });
        let response = app
            .clone()
            .oneshot(
                HttpRequest::post("/api/investigations")
                    .header("content-type", "application/json")
                    .body(body.to_string())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert!(state.jobs.lock().unwrap().is_empty());

        let legacy = serde_json::json!({
            "investigation": {"budget": {"max_steps_per_trace": 2}},
            "put": {"id": "x", "template": "t", "design_goals": "g", "tools": []},
            "scenario": {"world": "Fixture world."},
            "tags": {"label": "legacy"}
        });
        let response = app
            .oneshot(
                HttpRequest::post("/api/investigations")
                    .header("content-type", "application/json")
                    .body(legacy.to_string())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert!(state.jobs.lock().unwrap().is_empty());
    }

    #[test]
    fn provenance_attributes_use_resolved_settings_and_stable_empty_workspace_hash() {
        let empty = attributes::workspace_hash(&Workspace::empty());
        let mut renamed = put("cosmetic-id-only");
        let original = system_attributes(
            "zai_coding::glm-5.2",
            "zai_coding::glm-5.2",
            None,
            Some(ThinkingLevel::None),
            &renamed,
            &empty,
            BTreeMap::new(),
        );
        renamed.id = "renamed".into();
        let again = system_attributes(
            "zai_coding::glm-5.2",
            "zai_coding::glm-5.2",
            None,
            Some(ThinkingLevel::None),
            &renamed,
            &attributes::workspace_hash(&Workspace::empty()),
            BTreeMap::new(),
        );
        assert_eq!(original["put_model"], "zai_coding::glm-5.2");
        assert_eq!(original["put_thinking"], "provider_default");
        assert_eq!(original["sim_thinking"], "none");
        assert_eq!(original["prompt_hash"], again["prompt_hash"]);
        assert_eq!(original["workspace_hash"], again["workspace_hash"]);
    }

    #[test]
    fn simulator_settings_resolve_from_the_scenario_and_reject_old_controls() {
        // Lua runs exactly when the scenario supplies an implementation, and
        // the settings carry only its limits. There is no enable switch.
        let definition = definition_with_tools(vec![scenario_tool(
            "lookup",
            Some("return function() return {response=1} end"),
        )]);
        let runtime = prompt_explore::simulate::ScenarioRuntime::from_definition(
            &definition,
            Workspace::empty(),
        );
        assert_eq!(
            runtime.simulator.lua_simulation.unwrap().max_instructions,
            prompt_explore::model::lua::LuaOptions::default().max_instructions
        );
        let llm_only = definition_with_tools(vec![scenario_tool("lookup", None)]);
        assert!(
            prompt_explore::simulate::ScenarioRuntime::from_definition(
                &llm_only,
                Workspace::empty()
            )
            .simulator
            .lua_simulation
            .is_none()
        );

        // A scenario's Lua limits are honored, and remain validated.
        let mut limited = definition_with_tools(vec![scenario_tool(
            "lookup",
            Some("return function() return {response=1} end"),
        )]);
        limited.simulation.lua = Some(prompt_explore::model::lua::LuaOptions {
            max_instructions: 12345,
            ..Default::default()
        });
        let runtime = prompt_explore::simulate::ScenarioRuntime::from_definition(
            &limited,
            Workspace::empty(),
        );
        assert_eq!(
            runtime.simulator.lua_simulation.unwrap().max_instructions,
            12345
        );
        let mut unbounded = limited.clone();
        unbounded.simulation.lua = Some(prompt_explore::model::lua::LuaOptions {
            max_duration_ms: 10_001,
            ..Default::default()
        });
        let error = unbounded.validate().unwrap_err();
        assert!(error.contains("max_duration_ms"), "{error}");
        assert!(error.contains("must not exceed"), "{error}");

        // The investigation's controls no longer accept simulator knobs; the
        // error says where they went instead of silently overriding.
        let migrated: ConversationControls = serde_json::from_value(
            serde_json::json!({"sim_max_repair_attempts": 3, "max_workspace_turns": 5}),
        )
        .unwrap();
        let problem = conversation_controls_problem(&migrated).unwrap();
        assert!(problem.contains("sim_max_repair_attempts"), "{problem}");
        assert!(
            problem.contains("simulation.max_repair_attempts"),
            "{problem}"
        );
        assert!(
            problem.contains("POST /api/scenarios/{id}/fork"),
            "{problem}"
        );
    }

    #[test]
    fn default_repair_budget_is_resolved_and_reported() {
        let definition = definition_with_tools(vec![scenario_tool("lookup", None)]);
        let runtime = prompt_explore::simulate::ScenarioRuntime::from_definition(
            &definition,
            Workspace::empty(),
        );
        assert_eq!(runtime.simulator.max_repair_attempts, 20);
        let controls = ConversationControls::default();
        let (_, resolved) = resolved_conversation_controls(
            &controls,
            &runtime,
            &prompt_explore::simulate::WorkspaceToolLimits::default(),
        );
        assert_eq!(
            serde_json::to_value(resolved).unwrap()["simulator"]["sim_max_repair_attempts"],
            20
        );
    }

    #[test]
    fn put_controls_override_runner_defaults_and_invalid_values_are_refused() {
        let definition = definition_with_tools(vec![scenario_tool("lookup", None)]);
        let runtime = prompt_explore::simulate::ScenarioRuntime::from_definition(
            &definition,
            Workspace::empty(),
        );
        let controls = ConversationControls {
            put_temperature: Some(0.2),
            put_max_tokens: Some(111),
            ..ConversationControls::default()
        };
        let (runner, resolved) = resolved_conversation_controls(
            &controls,
            &runtime,
            &prompt_explore::simulate::WorkspaceToolLimits::default(),
        );
        assert_eq!(runner.put_temperature, Some(0.2));
        assert_eq!(runner.put_max_tokens, Some(111));
        assert_eq!(resolved.put_temperature, Some(0.2));
        assert_eq!(resolved.put_max_tokens, Some(111));
        assert!(conversation_controls_problem(&controls).is_none());

        let invalid = ConversationControls {
            put_max_tokens: Some(0),
            put_temperature: Some(-1.0),
            ..ConversationControls::default()
        };
        let problem = conversation_controls_problem(&invalid).unwrap();
        assert!(problem.contains("put_max_tokens"), "{problem}");
        assert!(problem.contains("put_temperature"), "{problem}");
    }

    fn scenario_tool(
        name: &str,
        lua_source: Option<&str>,
    ) -> prompt_explore::model::scenario::ScenarioTool {
        prompt_explore::model::scenario::ScenarioTool {
            name: name.into(),
            description: format!("{name} tool"),
            parameters: serde_json::json!({"type": "object"}),
            side_effect: prompt_explore::model::SideEffect::Read,
            example_responses: vec![],
            lua_source: lua_source.map(str::to_owned),
        }
    }

    fn definition_with_tools(
        tools: Vec<prompt_explore::model::scenario::ScenarioTool>,
    ) -> prompt_explore::model::scenario::ScenarioDefinition {
        prompt_explore::model::scenario::ScenarioDefinition {
            world: "Fixture world.".into(),
            input_domain: HashMap::new(),
            user_message: None,
            simulator_notes: String::new(),
            tools,
            simulation: Default::default(),
        }
    }

    #[test]
    fn thinking_level_problem_checks_each_role_independently() {
        let req = |model: &str,
                   _sim_model: Option<&str>,
                   put_level: Option<ThinkingLevel>,
                   _sim_level: Option<ThinkingLevel>| {
            InvestigateRequest {
                investigation: Investigation {
                    reason: None,
                    budget: Budget {
                        max_steps_per_trace: 2,
                        max_tokens: None,
                    },
                },
                put: put("x"),
                scenario_id: "scn-fixture".into(),
                scenario_revision: None,
                resolved_inputs: None,
                put_model: Some(model.into()),
                put_thinking_level: put_level,
                conversation_controls: ConversationControls::default(),
                scenario: None,
                scenarios: None,
                sim_model: None,
                sim_thinking_level: None,
                attributes: BTreeMap::new(),
            }
        };
        let (put_model, sim_model) =
            resolved_models(&req("open_router::custom-put", None, None, None));
        assert_eq!(put_model, "open_router::custom-put");
        assert_eq!(sim_model, MODEL, "put_model must not select the simulator");

        // No levels set: never a problem, whatever the models.
        assert!(
            thinking_level_problem(
                &req(
                    "bedrock_sigv4::global.openai.gpt-5.6-luna",
                    None,
                    None,
                    None
                ),
                "zai"
            )
            .is_none()
        );
        // OpenRouter PUT + zai sim: both mappable.
        assert!(
            thinking_level_problem(
                &req(
                    "open_router::openai/gpt-5.6-luna",
                    Some("zai_coding::glm-5.3"),
                    Some(ThinkingLevel::High),
                    Some(ThinkingLevel::None)
                ),
                "zai"
            )
            .is_none()
        );
        // Bedrock OpenAI settings are mapped independently for both roles.
        assert!(
            thinking_level_problem(
                &req(
                    "bedrock_sigv4::global.openai.gpt-6-astra",
                    Some("bedrock_sigv4::us.openai.gpt-5.6-luna"),
                    Some(ThinkingLevel::High),
                    Some(ThinkingLevel::None),
                ),
                "zai",
            )
            .is_none()
        );
        // Keyword validation belongs to the provider, not this mapping check.
        assert!(
            thinking_level_problem(
                &req(
                    "openai.gpt-6-astra",
                    None,
                    Some(ThinkingLevel::Minimal),
                    None
                ),
                "bedrock",
            )
            .is_none()
        );
        // Sim role on a different provider is checked against ITS model:
        // PUT fine on open_router, sim rejected on bedrock meta.*.
        let err = thinking_level_problem(
            &req(
                "bedrock_sigv4::global.meta.llama3-1-70b",
                None,
                Some(ThinkingLevel::Low),
                None,
            ),
            "zai",
        )
        .expect("a thinking level on a model without one must be rejected");
        assert!(err.contains("put_thinking_level"), "{err}");
        // Bedrock anthropic profile ids are supported (genai maps them
        // to a thinking budget).
        assert!(
            thinking_level_problem(
                &req(
                    "bedrock_sigv4::global.anthropic.claude-opus-5",
                    Some("bedrock_sigv4::eu.anthropic.claude-sonnet-5"),
                    Some(ThinkingLevel::Medium),
                    Some(ThinkingLevel::None)
                ),
                "zai"
            )
            .is_none()
        );
        // A bare name qualifies through the server's default provider.
        assert!(
            thinking_level_problem(
                &req("openai.gpt-5.6-luna", None, Some(ThinkingLevel::High), None),
                "bedrock",
            )
            .is_none()
        );
        let err = thinking_level_problem(
            &req("gpt-5.6-luna", None, Some(ThinkingLevel::High), None),
            "bedrock",
        )
        .expect("bare name under bedrock default must be checked as bedrock");
        assert!(err.contains("bedrock"), "{err}");
    }

    #[test]
    fn unknown_thinking_level_word_is_a_parse_error() {
        // The vocabulary fails fast at deserialization, before any
        // provider logic runs.
        let bad = r#"{"scenario_id": "scn-x", "investigation": {"budget": {"max_steps_per_trace": 2}}, "put": {"id": "x", "template": "t", "design_goals": "g"}, "put_thinking_level": "ultra"}"#;
        assert!(serde_json::from_str::<InvestigateRequest>(bad).is_err());
        let good = bad.replace("\"ultra\"", "\"xhigh\"");
        let req: InvestigateRequest = serde_json::from_str(&good).unwrap();
        assert_eq!(req.put_thinking_level, Some(ThinkingLevel::Xhigh));
        // The simulator's thinking level is the scenario's, not the request's.
        assert!(req.sim_thinking_level.is_none());
    }

    #[tokio::test]
    async fn post_requires_one_scenario_and_rejects_legacy_scenarios_as_unknown() {
        let state = test_state();
        let app = build_app(state);
        let body = serde_json::json!({
            "scenario_id": "scn-whatever",
            "investigation": {"budget": {"max_steps_per_trace": 1}},
            "put": {"id": "x", "template": "t", "design_goals": "g"},
            "scenario": {"world": "Fixture world."},
            "scenarios": [{"world": "legacy world"}]
        });
        let response = app
            .oneshot(
                HttpRequest::post("/api/investigations")
                    .header("content-type", "application/json")
                    .body(body.to_string())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = axum::body::to_bytes(response.into_body(), 1 << 20)
            .await
            .unwrap();
        let text = String::from_utf8_lossy(&body);
        assert!(text.contains("POST /api/scenarios"), "{text}");
        assert!(text.contains("scenario_id"), "{text}");
    }

    #[tokio::test]
    async fn create_investigation_rejects_unsupported_thinking_level_with_400() {
        let state = test_state();
        let app = build_app(state.clone());
        let mut body = investigation_body(&state);
        body["put_model"] = serde_json::json!("bedrock_sigv4::global.meta.llama3-1-70b");
        body["put_thinking_level"] = serde_json::json!("high");
        let res = app
            .clone()
            .oneshot(
                HttpRequest::post("/api/investigations")
                    .header("content-type", "application/json")
                    .body(serde_json::to_string(&body).unwrap())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let err = v["error"].as_str().unwrap();
        assert!(err.contains("put_thinking_level"), "{err}");
        assert!(err.contains("not supported"), "{err}");
    }

    #[tokio::test]
    async fn create_investigation_accepts_bedrock_openai_thinking_levels() {
        let state = test_state();
        let app = build_app(state.clone());
        for model in [
            "bedrock_sigv4::openai.gpt-oss-20b-1:0",
            "bedrock_sigv4::us.openai.gpt-5.6-luna",
            "bedrock_sigv4::global.openai.gpt-6-astra",
        ] {
            let mut body = investigation_body(&state);
            body["put_model"] = serde_json::json!(model);
            body["put_thinking_level"] = serde_json::json!("high");
            let res = app
                .clone()
                .oneshot(
                    HttpRequest::post("/api/investigations")
                        .header("content-type", "application/json")
                        .body(body.to_string())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(res.status(), StatusCode::ACCEPTED, "{model}");
            let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
                .await
                .unwrap();
            let created: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(created["attributes"]["put_model"], model);
            // The simulator's settings come from the scenario, not the request.
            assert_eq!(created["attributes"]["sim_thinking"], "provider_default");
            assert_eq!(created["attributes"]["scenario_revision"], "1");
            assert_eq!(
                created["attributes"]["prompt_hash"].as_str().unwrap().len(),
                64
            );
            assert_eq!(
                created["attributes"]["workspace_hash"]
                    .as_str()
                    .unwrap()
                    .len(),
                64
            );
        }
    }

    #[tokio::test]
    async fn a_workspace_larger_than_8_mib_is_accepted_on_the_scenario_route() {
        // The scenario route carries the multi-part body limit; the archive is
        // checked by the zip-specific caps (compressed and decompressed), not
        // by an unrelated 8 MiB default that would reject valid archives early.
        let mut archive = Vec::new();
        {
            let mut writer = zip::ZipWriter::new(std::io::Cursor::new(&mut archive));
            let options = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            writer.start_file("large.bin", options).unwrap();
            writer.write_all(&vec![b'x'; 9 * 1024 * 1024]).unwrap();
            writer.finish().unwrap();
        }
        assert!(archive.len() > 8 * 1024 * 1024);
        assert!(archive.len() < workspace_compressed_limit());

        let request = serde_json::json!({
            "scenario": {"world": "Fixture world.", "tools": []}
        })
        .to_string();
        let boundary = "prompt-explore-large-workspace-test";
        let mut body = Vec::with_capacity(request.len() + archive.len() + 1024);
        body.extend_from_slice(
            format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"request\"\r\nContent-Type: application/json\r\n\r\n{request}\r\n--{boundary}\r\nContent-Disposition: form-data; name=\"workspace\"; filename=\"workspace.zip\"\r\nContent-Type: application/zip\r\n\r\n"
            )
            .as_bytes(),
        );
        body.extend_from_slice(&archive);
        body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
        assert!(body.len() > 8 * 1024 * 1024);

        let state = test_state();
        let app = build_app(state.clone());
        let res = app
            .oneshot(
                HttpRequest::post("/api/scenarios")
                    .header(
                        "content-type",
                        format!("multipart/form-data; boundary={boundary}"),
                    )
                    .body(axum::body::Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = res.status();
        let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
            .await
            .unwrap();
        let response: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(status, StatusCode::CREATED, "{response}");
        let id = response["id"].as_str().unwrap();
        let record = state.scenarios.lock().unwrap();
        assert_eq!(record.get(id).unwrap().workspace.file_count(), 1);
    }

    async fn json_request(
        app: &Router,
        method: &str,
        uri: &str,
        body: serde_json::Value,
    ) -> (StatusCode, serde_json::Value) {
        let res = app
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .method(method)
                    .uri(uri)
                    .header("content-type", "application/json")
                    .body(body.to_string())
                    .unwrap(),
            )
            .await
            .unwrap();
        let code = res.status();
        let bytes = axum::body::to_bytes(res.into_body(), 1 << 22)
            .await
            .unwrap();
        (
            code,
            serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
        )
    }

    /// Poll a probe until it stops running (probes are asynchronous).
    async fn await_probe(app: &Router, scenario: &str, probe: &str) -> serde_json::Value {
        for _ in 0..200 {
            let (_, view) = json_request(
                app,
                "GET",
                &format!("/api/scenarios/{scenario}/simulations/{probe}"),
                serde_json::json!({}),
            )
            .await;
            if view["status"] != "running" {
                return view;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("probe never finished");
    }

    #[tokio::test]
    async fn scenario_lifecycle_over_http_edits_locks_forks_and_cascades() {
        let state = test_state();
        let app = build_app(state.clone());
        let body = serde_json::json!({
            "scenario": {"world": "first world", "tools": [], "input_domain": {}},
            "label": "trial"
        });
        let (code, created) = json_request(&app, "POST", "/api/scenarios", body).await;
        assert_eq!(code, StatusCode::CREATED, "{created}");
        let id = created["id"].as_str().unwrap().to_string();
        assert_eq!(created["revision"], 1);

        // Readable, editable, no dependents yet.
        let (code, view) = json_request(
            &app,
            "GET",
            &format!("/api/scenarios/{id}"),
            serde_json::json!({}),
        )
        .await;
        assert_eq!(code, StatusCode::OK);
        assert_eq!(view["editable"], true);
        assert_eq!(view["label"], "trial");
        assert_eq!(view["definition"]["world"], "first world");

        // An edit needs the revision the caller read.
        let patch = serde_json::json!({
            "expected_revision": 1,
            "scenario": {"world": "second world", "tools": [], "input_domain": {}}
        });
        let (code, edited) =
            json_request(&app, "PATCH", &format!("/api/scenarios/{id}"), patch).await;
        assert_eq!(code, StatusCode::OK, "{edited}");
        assert_eq!(edited["revision"], 2);
        assert_ne!(edited["definition_hash"], created["definition_hash"]);

        // The same (now stale) revision is refused rather than silently applied.
        let stale = serde_json::json!({
            "expected_revision": 1,
            "scenario": {"world": "third world", "tools": [], "input_domain": {}}
        });
        let (code, error) =
            json_request(&app, "PATCH", &format!("/api/scenarios/{id}"), stale).await;
        assert_eq!(code, StatusCode::CONFLICT, "{error}");
        assert!(
            error["error"].as_str().unwrap().contains("revision 2"),
            "{error}"
        );

        // An accepted investigation pins it: the edit is refused with guidance.
        let investigation_id = "inv-pinning";
        state
            .scenarios
            .lock()
            .unwrap()
            .attach_investigation(&id, investigation_id)
            .unwrap();
        let (code, view) = json_request(
            &app,
            "GET",
            &format!("/api/scenarios/{id}"),
            serde_json::json!({}),
        )
        .await;
        assert_eq!(code, StatusCode::OK);
        assert_eq!(view["editable"], false);
        assert_eq!(view["investigation_ids"][0], investigation_id);
        let (code, error) = json_request(
            &app,
            "PATCH",
            &format!("/api/scenarios/{id}"),
            serde_json::json!({
                "expected_revision": 2,
                "scenario": {"world": "blocked", "tools": [], "input_domain": {}}
            }),
        )
        .await;
        assert_eq!(code, StatusCode::CONFLICT, "{error}");
        assert!(error["error"].as_str().unwrap().contains("fork"), "{error}");

        // A running investigation blocks deletion outright, even with cascade:
        // a run cannot be cancelled, so deleting it would discard live spending.
        let (code, error) = json_request(
            &app,
            "DELETE",
            &format!("/api/scenarios/{id}?cascade=true"),
            serde_json::json!({}),
        )
        .await;
        assert_eq!(code, StatusCode::CONFLICT, "{error}");
        assert!(
            error["error"].as_str().unwrap().contains("running"),
            "{error}"
        );

        // Once it has stopped, the reference still pins the scenario (the
        // definition it ran must not change) and a plain delete names the
        // dependents instead of removing them.
        state
            .scenarios
            .lock()
            .unwrap()
            .finish_investigation(investigation_id);
        let (code, error) = json_request(
            &app,
            "DELETE",
            &format!("/api/scenarios/{id}"),
            serde_json::json!({}),
        )
        .await;
        assert_eq!(code, StatusCode::CONFLICT, "{error}");
        assert!(
            error["error"].as_str().unwrap().contains("cascade=true"),
            "{error}"
        );

        // A fork is editable immediately and carries the correction note.
        let (code, forked) = json_request(
            &app,
            "POST",
            &format!("/api/scenarios/{id}/fork"),
            serde_json::json!({
                "correction": {"reason": "grep matched nested paths"},
                "label": "corrected"
            }),
        )
        .await;
        assert_eq!(code, StatusCode::CREATED, "{forked}");
        let fork_id = forked["id"].as_str().unwrap().to_string();
        let (_, fork_view) = json_request(
            &app,
            "GET",
            &format!("/api/scenarios/{fork_id}"),
            serde_json::json!({}),
        )
        .await;
        assert_eq!(fork_view["editable"], true);
        assert_eq!(fork_view["correction"]["scenario_id"], id);
        assert_eq!(fork_view["correction"]["revision"], 2);
        assert_eq!(
            fork_view["correction"]["reason"],
            "grep matched nested paths"
        );
        assert_eq!(fork_view["label"], "corrected");

        // Cascade removes the scenario and reports what depended on it.
        let (code, deleted) = json_request(
            &app,
            "DELETE",
            &format!("/api/scenarios/{id}?cascade=true"),
            serde_json::json!({}),
        )
        .await;
        assert_eq!(code, StatusCode::OK, "{deleted}");
        assert_eq!(deleted["cascade_investigations"][0], investigation_id);
        let (code, _) = json_request(
            &app,
            "GET",
            &format!("/api/scenarios/{id}"),
            serde_json::json!({}),
        )
        .await;
        assert_eq!(code, StatusCode::NOT_FOUND);
        // The fork survives its predecessor.
        let (code, _) = json_request(
            &app,
            "GET",
            &format!("/api/scenarios/{fork_id}"),
            serde_json::json!({}),
        )
        .await;
        assert_eq!(code, StatusCode::OK);
    }

    #[tokio::test]
    async fn a_fully_lua_probe_runs_without_a_provider_client() {
        // The flagship workflow: register a world whose tool is implemented in
        // Lua, then test it over HTTP on a server with NO provider credentials.
        let state = test_state();
        assert!(state.client.is_none());
        let app = build_app(state.clone());
        let body = serde_json::json!({
            "scenario": {
                "world": "One record exists.",
                "input_domain": {},
                "user_message": "go",
                "tools": [{
                    "name": "lookup",
                    "description": "Return the record for an id.",
                    "parameters": {
                        "type": "object",
                        "properties": {"id": {"type": "string"}},
                        "required": ["id"],
                        "additionalProperties": false
                    },
                    "side_effect": "read",
                    "lua_source": "return function(args, ctx) return {response = {id = args.id, found = true}} end"
                }]
            }
        });
        let (code, created) = json_request(&app, "POST", "/api/scenarios", body).await;
        assert_eq!(code, StatusCode::CREATED, "{created}");
        let id = created["id"].as_str().unwrap().to_string();

        // Bad syntax is refused at registration with the tool named.
        let broken = serde_json::json!({
            "scenario": {
                "world": "w",
                "tools": [{
                    "name": "lookup",
                    "description": "d",
                    "parameters": {"type": "object"},
                    "side_effect": "read",
                    "lua_source": "return function( this is not lua"
                }]
            }
        });
        let (code, error) = json_request(&app, "POST", "/api/scenarios", broken).await;
        assert_eq!(code, StatusCode::BAD_REQUEST, "{error}");
        assert!(
            error["error"].as_str().unwrap().contains("lookup"),
            "{error}"
        );

        // Run a probe: two calls in one session, plus a schema-invalid one.
        let (code, submitted) = json_request(
            &app,
            "POST",
            &format!("/api/scenarios/{id}/simulations"),
            serde_json::json!({
                "tool_calls": [
                    {"name": "lookup", "args": {"id": "A-1"}},
                    {"name": "lookup", "args": {"nope": 1}},
                    {"name": "lookup", "args": {"id": "A-2"}}
                ],
                "reason": "does the handler echo ids?"
            }),
        )
        .await;
        assert_eq!(code, StatusCode::ACCEPTED, "{submitted}");
        let probe_id = submitted["id"].as_str().unwrap().to_string();
        let probe = await_probe(&app, &id, &probe_id).await;
        assert_eq!(probe["status"], "done", "{probe}");
        assert_eq!(probe["stop_reason"], "completed");
        assert_eq!(probe["scenario_revision"], 1);
        let calls = probe["calls"].as_array().unwrap();
        assert_eq!(calls.len(), 3);
        assert_eq!(calls[0]["response"]["id"], "A-1");
        assert_eq!(calls[0]["lua_execution"]["outcome"], "computed");
        assert_eq!(calls[0]["lua_execution"]["tool"], "lookup");
        assert_eq!(
            calls[0]["lua_execution"]["source_hash"]
                .as_str()
                .unwrap()
                .len(),
            64
        );
        // Schema-invalid arguments are in-band errors, exactly as in a run —
        // and they never reach the implementation.
        assert!(
            calls[1]["response"]
                .as_str()
                .unwrap()
                .contains("invalid arguments"),
            "{}",
            calls[1]["response"]
        );
        assert!(calls[1]["lua_execution"].is_null());
        assert_eq!(calls[2]["response"]["id"], "A-2");

        // A stale expected_revision is refused before anything runs.
        let (code, error) = json_request(
            &app,
            "POST",
            &format!("/api/scenarios/{id}/simulations"),
            serde_json::json!({
                "tool_calls": [{"name": "lookup", "args": {"id": "A-3"}}],
                "expected_revision": 7
            }),
        )
        .await;
        assert_eq!(code, StatusCode::CONFLICT, "{error}");

        // An offending tool call count is refused.
        let (code, error) = json_request(
            &app,
            "POST",
            &format!("/api/scenarios/{id}/simulations"),
            serde_json::json!({"tool_calls": []}),
        )
        .await;
        assert_eq!(code, StatusCode::BAD_REQUEST, "{error}");

        // A delegation with no provider fails the CALL, not the server, and
        // keeps the completed calls as evidence.
        let (code, delegated) = json_request(
            &app,
            "POST",
            "/api/scenarios",
            serde_json::json!({
                "scenario": {
                    "world": "w",
                    "tools": [{
                        "name": "lookup",
                        "description": "d",
                        "parameters": {"type": "object"},
                        "side_effect": "read",
                        "lua_source": "return function() PleaseSimulateException('needs the model') end"
                    }]
                }
            }),
        )
        .await;
        assert_eq!(code, StatusCode::CREATED);
        let llm_only = delegated["id"].as_str().unwrap();
        let (_, probe) = json_request(
            &app,
            "POST",
            &format!("/api/scenarios/{llm_only}/simulations"),
            serde_json::json!({"tool_calls": [{"name": "lookup", "args": {}}]}),
        )
        .await;
        let probe_id = probe["id"].as_str().unwrap();
        let probe = await_probe(&app, llm_only, probe_id).await;
        assert_eq!(probe["status"], "failed");
        assert_eq!(probe["stop_reason"], "runtime_failure");
        // The call itself is retained, with the reason it could not be rendered.
        assert_eq!(probe["calls"].as_array().unwrap().len(), 1);
        assert!(
            probe["calls"][0]["error"]
                .as_str()
                .unwrap()
                .contains("no LLM client"),
            "{probe}"
        );
        assert!(
            probe["error"].as_str().unwrap().contains("no LLM client"),
            "{probe}"
        );
    }

    #[tokio::test]
    async fn investigations_require_a_registered_scenario_and_reject_put_tools() {
        let state = test_state();
        let app = build_app(state.clone());
        let body = serde_json::json!({
            "scenario_id": "scn-does-not-exist",
            "investigation": {"budget": {"max_steps_per_trace": 1}},
            "put": {"id": "x", "template": "t", "design_goals": "g"}
        });
        let (code, error) = json_request(&app, "POST", "/api/investigations", body).await;
        assert_eq!(code, StatusCode::NOT_FOUND, "{error}");

        // The tool surface belongs to the scenario.
        let mut with_tools = investigation_body(&state);
        with_tools["put"] = serde_json::json!({
            "id": "x",
            "template": "t",
            "design_goals": "g",
            "tools": [{"name": "nope", "description": "d", "parameters": {}, "side_effect": "read"}]
        });
        let (code, error) = json_request(&app, "POST", "/api/investigations", with_tools).await;
        assert_eq!(code, StatusCode::BAD_REQUEST, "{error}");
        assert!(
            error["error"].as_str().unwrap().contains("scenario"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn patch_grades_rejects_reserved_and_bad_names() {
        let state = test_state();
        seed_done_job(&state, "job-1", "cancel-bot", 100, 2);
        let app = build_app(state);

        let (code, v) = patch_job(&app, "job-1", r#"{"grades": {"put_cost_usd": 1.0}}"#).await;
        assert_eq!(code, StatusCode::BAD_REQUEST);
        assert_eq!(v["problems"][0]["reason"], "reserved_axis_name");

        let (code, v) = patch_job(&app, "job-1", r#"{"grades": {"Bad Name": 1.0}}"#).await;
        assert_eq!(code, StatusCode::BAD_REQUEST);
        assert_eq!(v["problems"][0]["reason"], "bad_axis_name");

        let (code, v) = patch_job(&app, "no-such-job", r#"{"grades": {"x": 1.0}}"#).await;
        assert_eq!(code, StatusCode::NOT_FOUND);
        assert!(v["error"].as_str().unwrap().contains("lost on restart"));
    }

    #[tokio::test]
    async fn all_error_runs_are_failed_exclusions_not_zero_cost_candidates() {
        let state = test_state();
        seed_done_job(&state, "failed-run", "p", 0, 1);
        {
            let mut jobs = state.jobs.lock().unwrap();
            let job = jobs.get_mut("failed-run").unwrap();
            job.status = JobStatus::Failed;
            let result = job.result.as_mut().unwrap();
            result.trace = None;
            result.failure = Some(RunFailure {
                stage: "runner".into(),
                error: "fixture failure".into(),
            });
        }
        let app = build_app(state);
        let (status, _, body) = post_frontier(
            &app,
            "?format=json",
            r#"{"group_by":[],"axes":[{"name":"put_output_tokens","better":"lower"}]}"#,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let body: Value = serde_json::from_str(&body).unwrap();
        let point = &body["points"][0];
        assert!(point["values"].is_null());
        assert!(point["on_frontier"].is_null());
        assert_eq!(point["included"], serde_json::json!([]));
        assert_eq!(point["excluded"][0]["status"], "failed");
        assert_eq!(point["excluded"][0]["investigation"], "failed-run");
        assert_eq!(point["preliminary"], true);
    }

    #[tokio::test]
    async fn frontier_json_and_svg_round_trip() {
        let state = test_state();
        seed_done_job(&state, "v1", "cancel-bot", 1450, 2);
        seed_done_job(&state, "v2", "cancel-bot", 2300, 2);
        seed_done_job(&state, "v3", "cancel-bot", 3100, 3); // dominated by v2
        let app = build_app(state.clone());
        patch_job(&app, "v1", r#"{"grades": {"tone_of_voice": 0.4}}"#).await;
        patch_job(&app, "v2", r#"{"grades": {"tone_of_voice": 0.85}}"#).await;
        patch_job(&app, "v3", r#"{"grades": {"tone_of_voice": 0.75}}"#).await;

        for id in ["v1", "v2", "v3"] {
            state
                .jobs
                .lock()
                .unwrap()
                .get_mut(id)
                .unwrap()
                .attributes
                .insert("variant".into(), id.into());
        }
        let req_body = r#"{
            "group_by": ["variant"],
            "axes": [
                {"name": "put_output_tokens", "better": "lower"},
                {"name": "tone_of_voice", "better": "higher"}
            ]
        }"#;
        let (code, ct, body) = post_frontier(&app, "?format=json", req_body).await;
        assert_eq!(code, StatusCode::OK);
        assert!(ct.starts_with("application/json"));
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        let points = v["points"].as_array().unwrap();
        assert_eq!(
            points.len(),
            3,
            "one point for every variant attribute group"
        );
        assert!(points.iter().all(|point| point["attributes"].is_object()));
        assert!(points.iter().all(|point| point.get("tags").is_none()));
        // Mixed directions leave the cheap/low-tone and expensive/high-tone
        // variants on the frontier, while the third is dominated.
        assert_eq!(
            points.iter().filter(|p| p["on_frontier"] == true).count(),
            2
        );
        assert!(
            points
                .iter()
                .any(|p| !p["dominated_by"].as_array().unwrap().is_empty())
        );
        // Only the requested axes appear in a resolved group's values.
        let resolved = points.iter().find(|p| p["values"].is_object()).unwrap();
        assert!(resolved["values"].get("steps_per_trace_avg").is_none());

        let (code, ct, body) = post_frontier(&app, "?format=svg", req_body).await;
        assert_eq!(code, StatusCode::OK);
        assert!(ct.starts_with("image/svg+xml"), "ct={ct}");
        assert!(body.starts_with("<svg "));
        assert!(body.contains("<svg"));
    }

    #[tokio::test]
    async fn frontier_typed_problems_over_http() {
        let state = test_state();
        seed_done_job(&state, "v1", "cancel-bot", 1450, 2);
        state.jobs.lock().unwrap().insert(
            "still-running".into(),
            Job {
                status: JobStatus::Running,
                result: None,
                progress: Arc::new(Mutex::new(RunProgress::default())),
                put_tracker: None,
                sim_tracker: None,
                started_at: 0,
                finished_at: None,
                budget: Budget {
                    max_steps_per_trace: 6,
                    max_tokens: None,
                },
                assessment: None,
                reason: None,
                put: put("cancel-bot"),
                grades: BTreeMap::new(),
                scenario_id: "scn-fixture".into(),
                scenario_revision: 1,
                scenario_definition_hash: "fixture".into(),
                scenario: Scenario {
                    world: "Fixture world.".into(),
                    input_domain: HashMap::new(),
                    user_message: None,
                    simulator_notes: String::new(),
                },
                put_model: "zai_coding::glm-5.2".into(),
                sim_model: "zai_coding::glm-5.2".into(),
                put_thinking_level: None,
                sim_thinking_level: None,
                conversation_controls: ResolvedConversationControls::default(),
                workspace_files: 0,
                attributes: system_attributes(
                    "zai_coding::glm-5.2",
                    "zai_coding::glm-5.2",
                    None,
                    None,
                    &put("cancel-bot"),
                    &attributes::workspace_hash(&Workspace::empty()),
                    BTreeMap::new(),
                ),
            },
        );
        let app = build_app(state);

        let body = r#"{
            "group_by": ["put_model"],
            "axes": [
                {"name": "put_cost_usd", "better": "lower"},
                {"name": "tone_of_voice", "better": "higher"}
            ]
        }"#;
        let (code, _ct, body) = post_frontier(&app, "", body).await;
        // Every stored job is considered; incomplete/running groups are
        // successful, pollable evidence rather than a request-level 422.
        assert_eq!(code, StatusCode::OK);
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        let points = v["points"].as_array().unwrap();
        assert_eq!(points.len(), 1);
        assert_eq!(
            points[0]["investigations"],
            serde_json::json!(["still-running", "v1"])
        );
        assert_eq!(points[0]["preliminary"], true);
        assert!(points[0]["values"].is_null());
        assert!(
            points[0]["excluded"]
                .as_array()
                .unwrap()
                .iter()
                .any(|e| e["status"] == "running")
        );
    }

    #[tokio::test]
    async fn grouped_frontier_backlog_updates_same_group_after_patch() {
        let state = test_state();
        seed_done_job(&state, "complete", "cancel-bot", 100, 2);
        seed_done_job(&state, "later", "cancel-bot", 100, 2);
        {
            let mut jobs = state.jobs.lock().unwrap();
            let complete = jobs.get_mut("complete").unwrap();
            complete.attributes.insert("campaign".into(), "same".into());
            complete.grades.insert("quality".into(), 1.0);
            let later = jobs.get_mut("later").unwrap();
            later.attributes.insert("campaign".into(), "same".into());
            later.status = JobStatus::Running;
        }
        let app = build_app(state.clone());
        let body = r#"{"group_by":["campaign"],"axes":[{"name":"quality","better":"higher"}]}"#;
        let (code, _, first) = post_frontier(&app, "", body).await;
        assert_eq!(code, StatusCode::OK);
        let point = &serde_json::from_str::<serde_json::Value>(&first).unwrap()["points"][0];
        assert_eq!(point["included"], serde_json::json!(["complete"]));
        assert_eq!(point["preliminary"], true);
        assert_eq!(point["excluded"][0]["status"], "running");

        // Once the run completes it remains visible as an awaiting-grade
        // backlog member; the same PATCH that supplies the grade joins it to
        // the existing attribute group rather than creating a selected-job view.
        state.jobs.lock().unwrap().get_mut("later").unwrap().status = JobStatus::Done;
        let (code, _) = patch_job(&app, "later", r#"{"grades":{"quality":3}}"#).await;
        assert_eq!(code, StatusCode::OK);
        let (code, _, second) = post_frontier(&app, "", body).await;
        assert_eq!(code, StatusCode::OK);
        let point = &serde_json::from_str::<serde_json::Value>(&second).unwrap()["points"][0];
        assert_eq!(point["included"], serde_json::json!(["complete", "later"]));
        assert_eq!(point["excluded"], serde_json::json!([]));
        assert_eq!(point["preliminary"], false);
        assert_eq!(point["values"]["quality"], 2.0);
    }

    #[tokio::test]
    async fn grouped_frontier_rejects_legacy_selected_investigations_field() {
        let app = build_app(test_state());
        let (code, _, body) = post_frontier(
            &app,
            "",
            r#"{"investigations":["old-id"],"axes":[{"name":"quality","better":"higher"}]}"#,
        )
        .await;
        assert_eq!(code, StatusCode::BAD_REQUEST);
        assert!(body.contains("unknown field `investigations`"), "{body}");
    }

    #[tokio::test]
    async fn lua_reference_is_served_as_markdown_and_named_by_the_spec() {
        // The Lua handler contract is the one thing a caller cannot infer from
        // the endpoint list; the running server must be able to hand it over.
        let app = build_app(test_state());
        let res = app
            .clone()
            .oneshot(HttpRequest::get("/docs/lua").body(String::new()).unwrap())
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let ct = res
            .headers()
            .get(axum::http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_string();
        assert!(ct.starts_with("text/markdown"), "ct={ct}");
        let body = String::from_utf8(
            axum::body::to_bytes(res.into_body(), 1 << 20)
                .await
                .unwrap()
                .to_vec(),
        )
        .unwrap();
        for needle in [
            "return function(args, ctx)",
            "ctx.workspace.read",
            "ctx.workspace.grep",
            "ctx.workspace.write",
            "PleaseSimulateException",
            "lua_computed_calls",
        ] {
            assert!(body.contains(needle), "reference is missing {needle}");
        }
        // The spec points a spec-only caller at the same reference.
        let spec = ApiDoc::openapi();
        assert!(spec.paths.paths.contains_key("/docs/lua"));
        let spec_json = serde_json::to_value(&spec).unwrap();
        assert!(
            spec_json["info"]["description"]
                .as_str()
                .unwrap()
                .contains("GET /docs/lua")
        );
        assert!(spec_json["components"]["schemas"]["LuaWorkspaceCapability"].is_object());
    }

    #[tokio::test]
    async fn grouped_frontier_svg_renders_all_pending_backlog() {
        let state = test_state();
        seed_done_job(&state, "pending", "cancel-bot", 100, 2);
        state
            .jobs
            .lock()
            .unwrap()
            .get_mut("pending")
            .unwrap()
            .status = JobStatus::Running;
        let app = build_app(state);
        let (code, content_type, svg) = post_frontier(
            &app,
            "?format=svg",
            r#"{"axes":[{"name":"quality","better":"higher"},{"name":"clarity","better":"higher"}]}"#,
        ).await;
        assert_eq!(code, StatusCode::OK);
        assert!(content_type.starts_with("image/svg+xml"));
        assert!(svg.contains("no groups have a complete cohort"));
        assert!(svg.contains("pending / preliminary backlog"));
    }

    #[tokio::test]
    async fn grouped_frontier_stops_showing_deleted_jobs() {
        let state = test_state();
        seed_done_job(&state, "keep", "cancel-bot", 100, 2);
        seed_done_job(&state, "remove", "cancel-bot", 200, 2);
        for id in ["keep", "remove"] {
            let mut jobs = state.jobs.lock().unwrap();
            let job = jobs.get_mut(id).unwrap();
            job.attributes.insert("variant".into(), id.into());
            job.grades.insert("quality".into(), 1.0);
        }
        let app = build_app(state);
        let body = r#"{"group_by":["variant"],"axes":[{"name":"quality","better":"higher"}]}"#;
        let (code, _, before) = post_frontier(&app, "", body).await;
        assert_eq!(code, StatusCode::OK);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&before).unwrap()["points"]
                .as_array()
                .unwrap()
                .len(),
            2
        );

        let response = app
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .method("DELETE")
                    .uri("/api/investigations/remove")
                    .body(String::new())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let (code, _, after) = post_frontier(&app, "", body).await;
        assert_eq!(code, StatusCode::OK);
        let points = serde_json::from_str::<serde_json::Value>(&after).unwrap()["points"]
            .as_array()
            .unwrap()
            .clone();
        assert_eq!(points.len(), 1);
        assert_eq!(points[0]["investigations"], serde_json::json!(["keep"]));
    }

    #[tokio::test]
    async fn frontier_rejects_svg_with_non_two_axes() {
        let state = test_state();
        seed_done_job(&state, "v1", "cancel-bot", 1450, 2);
        let app = build_app(state);
        let (code, _ct, body) = post_frontier(
            &app,
            "?format=svg",
            r#"{"axes": [{"name": "put_output_tokens", "better": "lower"}]}"#, // 1 axis
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
        // carries a `<link rel="service-desc" href="/openapi.json">` element
        // in <head> (plus a visible footer line) so the spec is
        // discoverable from the body itself, not just the header. This
        // test guards against that marker being silently removed —
        // removing it re-breaks body-only consumers, which is easy to do
        // by accident since the header still works and hides the regression.
        assert!(
            INDEX_HTML.contains(r#"rel="service-desc" href="/openapi.json""#),
            "the / body must advertise the OpenAPI spec via a service-desc \
             link element; body-only consumers (most agent HTTP tools) cannot \
             see the Link header"
        );
    }
}
