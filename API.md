# prompt-explore API

Property-based testing for agent behavior. You author a reusable SCENARIO once (a test case: a world narrative, an input domain, a protagonist, the tool surface, and optional Lua implementations of those tools), develop and TEST its simulation with direct tool-call probes, and then run many investigations against it. Each investigation = one scenario revision + one execution of a caller-authored Lua workflow. There is one application form: workflow. Omit workflow.lua_source to use the default single-agent program; its params.prompt/model/controls names are conventions, not privileged schema fields. Custom Lua source returns function(params, ctx), and workflow.params is arbitrary JSON. ctx.render(text) fills sampled input_domain values; ctx.run_agent{prompt=...,model=...,input=...,name=...} runs one fresh agent conversation; ctx.call_tool(name,args) calls a scenario tool directly. Calls share world/workspace and host-enforced investigation budgets. Exact handoffs, partial failures and all usage remain evidence even when Lua discards outputs. Read GET /docs/workflow before authoring a program. TWO DISTINCT LUA CONTEXTS: scenario tool-handler Lua may delegate a tool response to the simulator on error; investigation WORKFLOW Lua NEVER delegates on error or resource exhaustion. run_agent is real execution of the tested model, never simulated. Workflow limit breaches stop/fail the program with partial evidence; catching an error cannot reset global caps. Direct orchestration calls live in evidence.workflow.tool_calls, NOT in agent turns; stage details live in evidence.workflow.invocations. The harness surfaces COMPLETE EVIDENCE — the world, input domain, resolved inputs, and the full trace of model turns — or explicit failure evidence. THE CALLER IS THE JUDGE: there is no in-harness verdict, and the optional `reason` is advisory framing for whoever reads the traces.   PROMPT-OPTIMIZATION CHECKPOINT: READ -> PATCH -> COMPARE -> EDIT. When helping a developer improve a prompt, finish this checkpoint for the current results BEFORE editing the prompt or launching its successor. This is the experiment loop, not end-of-session reporting. Recording comparable judgments now reduces cherry-picking and lets the user independently inspect your comparison in the dashboard. Neither numbers nor a frontier prove quality.  BEFORE RUNNING THE EXPERIMENT, agree what better means WITH the user: the quality axes, their scales, acceptable tradeoffs, which delivered outputs count, and the outcome of a NON-DELIVERING attempt (for example, empty output or a tool loop capped before delivering a review). If the user already supplied an acceptance rubric, use it; otherwise propose concrete metrics and ask for confirmation before calling them agreed. Do not wait for results to decide whether non-deliveries count, or silently omit them from the recommendation. Keep the rubric stable and store it in assessment.rubric; use measured put_cost_usd or elapsed_ms alongside quality, not instead of it.  Example caller-owned rubric for an audit with known positive findings: delivered_recall = real actionable paths reported in the delivered review / known true paths; no delivered findings means 0. Precision = real warnings / emitted warnings; with no warnings it is undefined, not automatically 0 or 1. Optionally track usable_report separately (1 for a usable review, otherwise 0, under an agreed definition; nonempty text alone is not sufficient). Two comparable completed attempts, one with full delivered recall and one with no review, have mean delivered_recall 0.5, not 1.0. A genuinely ungradable trace, such as one with unreliable simulation evidence, instead needs an explanatory assessment and absent grades, not an invented failure score.  For EACH completed investigation: (1) GET /api/investigations/{id}/evidence and read the actual conversation, stop reason and simulation fidelity. (2) Judge it against the agreed rubric. (3) Immediately SEND PATCH /api/investigations/{id} with grades AND an assessment explaining the evidence. Read the successful response and confirm it echoes your annotations (or re-GET to verify). Do not postpone PATCH until all prompt versions are finished. Scores in local JSON, notebook tables, a generated PATCH script, state_patch inside a simulated tool, and a promise to grade later do NOT record caller judgment on an investigation. If evidence cannot justify a grade, PATCH an assessment explaining why and clear any stale grade with null instead of manufacturing a score. Grade delivered behavior, not a proposed answer in thinking or a plausible final sentence alone.  Then SEND POST /api/frontier and READ its returned points before the next prompt edit. Example request: {"group_by":["application_hash","scenario_id","scenario_revision"],"axes":[{"name":"delivered_recall","better":"higher"},{"name":"put_cost_usd","better":"lower"}]}. Every requested axis must exist on a run for it to contribute. Adding undefined precision to this primary comparison would exclude the no-review attempt again: inspect precision in a separate, explicitly conditional view and report its denominator. Execution-failed jobs remain excluded even when graded; account for them explicitly in the recommendation too. Inspect points[].values, included, excluded, preliminary, on_frontier and dominated_by. Missing requested grades remain explicit backlog; measured-only frontiers need no grades but cannot establish quality. Pending means no common contributing cohort; preliminary means some members were excluded. Neither means the candidate failed or won. Check corpus coverage and comparable provenance, not just the non-dominated marker. Explain the observed tradeoff and exclusions to the user, THEN choose a prompt change targeting a demonstrated weakness. Re-run comparable scenarios and repeat this checkpoint.  Before saying an iteration is complete, check what actually happened: PATCH response confirmed? Frontier response read AFTER those judgments? Decision tied to its values and evidence? If not, do the missing calls now, or report the concrete blocker rather than claiming a validated improvement. This is caller workflow guidance, not a server-enforced gate; the API also supports exploratory trace reading without grades.  WORKED LOOP (the caller does every judgment):  1. POST /api/scenarios registers the world: `world`, `input_domain`, `user_message`, `simulator_notes`, `tools[]` (name/description/parameters/ side_effect, plus optional `lua_source`), and `simulation` settings (the simulator model, thinking level, limits, and Lua sandbox limits). Supply the initial workspace once, as the optional `workspace` .zip part of a multipart body (part `request` = the JSON, part `workspace` = the archive). The response `id` + `revision` are what you reference afterwards.  2. Develop the simulation BEFORE spending investigations. POST /api/scenarios/{id}/simulations with an ordered `tool_calls` list; poll GET /api/scenarios/{id}/simulations/{probe_id}. Each call is rendered through the SAME engine an investigation uses (same argument validation, same Lua sandbox and rollback, same LLM delegation), so what you test is what runs. Read every call's `response`, `lua_execution` (computed vs delegated vs errored, with `source_hash`) and `workspace_ops`. Calls in one submission run in sequence in one session, so write/read consistency is testable; each submission starts from a fresh snapshot. A probe never invokes the application and never becomes a frontier candidate. You need no local Lua toolchain: the server parses, sandboxes, and executes the source.  3. Iterate: PATCH /api/scenarios/{id} with the complete new definition plus the `expected_revision` you read (a stale value is refused). Editing is allowed exactly while NO investigation references the scenario; you need no separate publish step — submitting an investigation pins it.  4. Check inputs too. A probe may pass `resolved_inputs` to pin the inputs for one test; omit it to sample them. Every run reports what it actually used in `resolved_inputs`.  5. POST /api/investigations with `scenario_id` (plus optional `scenario_revision` as a staleness guard and optional `resolved_inputs`), `workflow` (program source, opaque `params` and limits) and the investigation's budget/reason/attributes. The tool surface comes from the scenario and cannot be redefined by the workflow request. Submission pins the scenario immediately and atomically.  6. Poll GET /api/investigations/{id}, then read GET /api/investigations/{id}/evidence. Read execution.stop_reason, budget and timing first: done means a trace was recorded, not necessarily a final answer. Read turns in order, including EVERY tool_exchanges[].call AND response. The response is what the application observed; workspace_ops is only supporting provenance. lua_execution=computed means code executed, NOT that the response is faithful. A final correct answer can hide invalid root listings, false-empty searches, or invented files.  7. Record the judgment in the product. This is the step that makes the run comparable on judged axes: immediately PATCH the id with `grades` (one number per agreed axis) and `assessment` (summary, the `rubric` scale, and `evidence` entries naming the turn/exchange the judgment rests on). Example: {"grades":{"found_all":0.75,"precision":1.0},"assessment":{"summary":"Found three of four planted paths; missed the sanitizer bypass","rubric":"found_all: share of planted paths reported, 0..1; precision: 1 - share of reported paths that are not real","evidence":[{"turn":1,"exchange":0,"note":"cleared the highlight helper after reading stripTags; the surviving unclosed-tag payload never appears in the trace"}]}}. Clear stale grades in the SAME PATCH when an assessment invalidates them (grades:{"found_all":null}). Grades are caller-owned; the harness stores and compares them and NEVER substitutes a verdict of its own. 8. Fixing a simulation after traces exist: the scenario is pinned, so POST /api/scenarios/{id}/fork (optionally with a `correction` note naming the predecessor). The fork is editable and shares the initial workspace without re-uploading it. Every investigation reports scenario_id, scenario_revision and scenario_definition_hash, so you can tell exactly which traces ran the old definition and re-run only those.  9. READ THE FRONTIER BEFORE YOU CHANGE THE PROMPT. POST /api/frontier with `group_by` naming the variables you are comparing (application_hash, scenario_id, scenario_revision, or your own labels such as a prompt-version attribute; the reserved provenance attributes also include scenario_id/scenario_revision/scenario_hash, simulation_backend, step_budget and token_budget) and `axes` naming the metrics you agreed with your user (your judged grades plus measured ones like put_cost_usd, steps_per_trace_avg, elapsed_ms). This is the corpus-wide table of what you compared: inspect the returned `points`, not a nonexistent `groups` field. A point with null `values` has no contributing run with every requested axis. Inspect `excluded` for missing grades, running/failed runs or unavailable measured axes. Preliminary points participate in dominance but their exclusions limit the conclusion; missing grades are a review backlog, not a loss. All stored jobs remain candidates; a card filter never limits candidacy. The caller owns corpus comparability and grade scales. After reading it — and only then — change the prompt. Share state via URL-encoded query values (group_by, axes, attributes) — never a bearer token. Archiving: scenarios, investigations, grades and probes are all in memory and lost on restart. GET /api/scenarios/{id}/workspace exports the initial workspace inventory; a hash alone is not a reproducible workspace.  DESIGN INTENT — why it works this way:  • Scenarios are world SPECIFICATIONS, not instantiated data. A narrative pins what exists (inventory; facts, including NEGATIVE facts; completeness assertions; rendering rules) and the simulator lazily renders concrete tool responses from it. Materializing a full environment requires a closed world (enumerable, bounded, copyable); open worlds — web search, email, a payment network — can never be materialized, so a narrative is the only mechanism that generalizes. The optional workspace .zip is the container form of a closed world: upload it once with the scenario to hand the simulator authoritative bytes.  • Tool responses are SIMULATED from the narrative. By default the simulator LLM renders every response. A tool with `lua_source` is tried in the sandbox FIRST: a computed reply costs no model call, `PleaseSimulateException("reason")` delegates that one input, and a runtime error delegates too while keeping distinct error evidence and discarding its staged workspace writes. Code supplied is code run — there is no enable switch, and the harness never authors, repairs or rewrites the source. Code that executed is NOT proof it is faithful: read the responses against the world.  • The answer to simulation unreliability is TRANSPARENCY, not enforcement: every response is in the trace, the same narrative is visible, and a response that contradicts the stated facts is visible for the caller to catch. When simulation quality is insufficient, sharpen the narrative, fix the implementation, or use a stronger simulator model (which is set on the SCENARIO, because the environment is part of the test case).  • Reports clearly separate what was deterministic (validation, state patches, workspace bytes, usage, timing) from what was semantic (model output, rendered tool responses). The caller judges the semantic part.  • Phases: progress.phase is resolving_inputs or put_loop. There is no preparation phase; nothing is compiled or generated during a run.  THE SIMULATION WORKSPACE (optional, closed-world materialization). The optional `workspace` .zip is decompressed ENTIRELY IN MEMORY (never on disk) and seeds an in-memory filesystem the tool SIMULATOR consults. The simulator accesses it with four tools — read, write, list_dir, grep. The workspace is EPHEMERAL and per-run (every run gets a fresh copy of the scenario's seed; the agent under test never sees it — only tool responses). WHEN the simulator uses it is the world narrative's policy: state what the archive contains, where things live, and its completeness stance (closed: "these are ALL the files"; partial: "these are SOME files; simulate the rest"). Each tool exchange records the simulator's workspace operations (`workspace_ops`) so you can judge whether an answer was grounded in the uploaded files or invented. Caps: ≤ 50 MB compressed, ≤ 500 MB decompressed (overridable via PROMPT_EXPLORE_WORKSPACE_{COMPRESSED,DECOMPRESSED}_LIMIT); zip-slip entries and files in the reserved .prompt-explore namespace are rejected. WRITING A LUA IMPLEMENTATION. `GET /docs/lua` serves the handler reference from this server: the chunk contract, every `ctx.workspace` operation with its argument and return shape, the sandbox limits, and how a declined call, a runtime error and a missing implementation each delegate a single call to the simulator LLM. The `LuaWorkspaceCapability` schema states the same operations in the spec, and `POST /api/scenarios/{id}/simulations` tests them before you spend an investigation. After a run, `execution.lua_computed_calls` / `lua_fallback_calls` / `lua_error_calls` say whether your code actually served it.  AUTHENTICATION. The server is open by default. When PROMPT_EXPLORE_API_TOKEN is set (non-empty), every /api/* route EXCEPT /api/openapi.json requires an `Authorization: Bearer <token>` header (security scheme `api_token`). The web UI prompts for the token and stores it in localStorage.

Version: `0.6.1` — generated from `openapi.json`; do not edit by hand (see `scripts/dump-openapi.sh`).

## Endpoints

### `GET /`

Serve the web UI. Share a view using URL-encoded JSON query parameters: `group_by` is an array of attribute names, `axes` an array of {name,better}, and `attributes` an object of exact string matches for cards ONLY. Filtering cards never changes the all-jobs frontier. The UI copies/restores these view settings without storing a server-side selection. Never put tokens in URLs.

| Status | Response |
|---|---|
| `200` | Web UI (HTML) |

### `POST /api/frontier`

There is no investigation-selection list: every investigation the server holds is a candidate, and a card filter never limits candidacy. `group_by` chooses the provenance/campaign attributes that define one candidate point (default: put model, thinking setting, and behavior-only prompt hash), so two runs share a group exactly when their selected attribute values match; other properties may differ. Coordinates average requested axes over the SAME completed cohort having EVERY requested value. Measured axes (tokens, cost, steps, durations) need no caller grades; judged axes are the caller's PATCHed grades. An ungraded member remains visible in the group, and is excluded only when a requested grade is missing.  `points[].values = null` means NO member has the full requested set of values. Read `excluded` to distinguish awaiting grades from running/failed jobs and unavailable measured axes. PATCH justified missing grades (or clear unjustified ones and record why in assessment), then request the frontier again. Preliminary groups have exclusions but still participate in dominance when values exist; report those limitations rather than treating a provisional comparison as settled. A failed run (`result.failure` present, `result.trace` null) is a failed exclusion; a done job contributes its single trace. Judging trace adequacy remains the caller's responsibility, and the harness never substitutes a verdict of its own.

| Parameter | In | Type | Description |
|---|---|---|---|
| `format` | query | string | `json` (default) or `svg` |

Body: [`GroupedFrontierRequest`](#groupedfrontierrequest)

| Field | Type | Required | Description |
|---|---|---|---|
| `axes` | [`FrontierAxis`](#frontieraxis)[] | yes | Axes whose arithmetic means define Pareto dominance. Every included run has every requested value, so different axes never average different cohorts. |
| `group_by` | string[] | no | Attribute names that form a group. Omit for exactly `["application_hash", "scenario_id", "scenario_revision"]` — the identity of the submitted application AND of the world it ran against. Both always exist, so a default group is never keyed on an absent fact; two investigations share a group only when they ran the same program against the same scenario revision. Send `[]` for one group containing every current job. A job missing a requested key is retained in that key's explicit JSON-null group, never dropped. |


| Status | Response |
|---|---|
| `200` | Grouped frontier and exclusion evidence, including running/failed/awaiting-grades/unavailable members. Pending groups have null coordinates; poll investigations and resubmit after they finish or receive grades.: [`GroupedFrontierResponse`](#groupedfrontierresponse) |
| `400` | Malformed body or unknown ?format |
| `401` | Missing or invalid bearer token |
| `422` | Invalid grouping/axis request (for example bad attribute or axis name, duplicate axis, incompatible direction, or SVG arity).: [`FrontierError`](#frontiererror) |

### `GET /api/investigations`

List all jobs (for the dashboard). Running jobs first, then by recency. Returns summaries only — poll a job's id for full progress. Optionally pass `attributes` as a URL-encoded JSON object of string key/value pairs; every pair must exactly match a stored attribute (AND semantics). Omit it to list all jobs. This filters only this listing, never POST /api/frontier.

| Parameter | In | Type | Description |
|---|---|---|---|
| `attributes` | query | string | Optional URL-encoded JSON object of exact attribute string matches, e.g. `?attributes=%7B%22campaign%22%3A%22spring%22%7D`. Pairs are ANDed; omit to list all. Attribute keys use `^[a-z][a-z0-9_]{0,63}$`; malformed JSON/shapes/keys return 400. This does NOT filter POST /api/frontier. |

| Status | Response |
|---|---|
| `200` | All matching job summaries: [`JobSummary`](#jobsummary)[] |
| `400` | Malformed attributes query JSON, shape, or key |
| `401` | Missing or invalid bearer token |

### `POST /api/investigations`

Every HTTP investigation supplies `workflow`; omit its `lua_source` to use the default single-agent program. `workflow.params` is arbitrary JSON with no privileged keys. The default program happens to read `prompt`, `model` and optional `controls`, calls `ctx.render(prompt)`, then invokes one agent. Custom source returns function(params, ctx); call ctx.run_agent for each agent stage and ctx.call_tool for direct scenario-tool access. Each stage has a fresh conversation but all stages and direct calls share one simulation world and investigation-wide budget. See GET /docs/workflow for the full contract.  The program is part of the investigation, not the scenario. Compare one-stage and multi-stage programs using the same scenario_id and resolved_inputs; repeated submissions without explicit bindings sample inputs afresh. Submit separate investigations for experimental repetitions, then group by attributes.  Poll the returned id and read /api/investigations/{id}/evidence. For workflow runs inspect workflow.output and workflow.invocations (exact handoffs and ranges into flat turns), not merely the last agent completion. Partial failures, direct tool responses and all usage survive discarded outputs. The caller judges end-to-end behavior; done only means evidence was recorded.

Body: [`InvestigateRequest`](#investigaterequest)

| Field | Type | Required | Description |
|---|---|---|---|
| `attributes` | map&lt;string, string&gt; | no | Caller-owned campaign attributes. This field is literally `attributes`; there is no `tags` alias and unknown fields are rejected. Keys use `^[a-z][a-z0-9_]{0,63}$`; values are strings up to 1024 UTF-8 bytes. `label` is the special editable display label shown by the UI. POST rejects every system-owned key: `application_hash` is the content identity of the submitted workflow (source, params and limits); `sim_model`/`sim_thinking` are the SIMULATOR's resolved settings (the simulator belongs to the scenario); `workspace_hash` is SHA-256 of sorted uploaded workspace path/content pairs (including the stable empty-workspace hash); `simulation_backend` is `llm` or `lua`; `step_budget` and `token_budget` are decimal limits (unbounded token budget is `unlimited`); `scenario_id`/`scenario_revision`/`scenario_hash` identify the pinned world. There is no `put_model`: a program may invoke several models, so record a model name as a caller-owned attribute if you want to group by it. All system attributes are immutable provenance; group by them explicitly when comparing execution configurations. |
| `investigation` | [`Investigation`](#investigation) | yes |  |
| `resolved_inputs` | object? | no | Optional explicit bindings for the scenario's declared inputs (replay a specific sample instead of drawing a new one). Supply every declared key or omit entirely. |
| `scenario_id` | string | yes | The scenario to run, BY REFERENCE. The referenced definition's tool contracts become this run's tool surface. |
| `scenario_revision` | integer? | no | Optional guard against editing races: refuse to run unless the scenario is still at this revision. The referenced scenario is pinned by this submission, so it cannot be edited afterwards either. |
| `workflow` | [`WorkflowProgram`](#workflowprogram) | no | Caller-authored Lua orchestration and opaque JSON parameters. The program belongs to this investigation, NOT to the scenario. Omit it (or its `lua_source`) to run the default single-stage program. Each ctx.run_agent call runs a fresh agent conversation in the same scenario world; ctx.call_tool invokes that world's tools directly. All work shares investigation-wide budgets and host-owned evidence.  The default program's ordinary parameter convention is params.prompt (system text, `{{input_domain}}` values filled by ctx.render), params.model (qualified model name) and optional params.controls. Nothing about those key names is privileged: they are simply what the default program reads. Custom source returns function(params,ctx); run_agent options include name, prompt, model, input (string), tools (optional name subset), controls={thinking,temperature,max_tokens}, budget={max_steps,max_tokens}. ctx.input is the scenario user message; ctx.resolved_inputs holds its sampled bindings; ctx.render(text) fills `{{variable}}` placeholders from that sample. Handoffs are literal, never regenerated by the simulator. See GET /docs/workflow for the executable contract and examples. |


| Status | Response |
|---|---|
| `202` | Investigation job created: [`JobCreated`](#jobcreated) |
| `400` | Malformed request, unknown legacy fields, invalid attributes, or invalid Lua source/limits. Runtime Lua/provider errors are retained in job failure evidence; poll the job. |
| `401` | Missing or invalid bearer token |

### `DELETE /api/investigations/{id}`

Delete an investigation: remove the job — its traces, grades, attributes, and progress — from the server's memory. Irreversible: the evidence is gone (a re-run means POSTing a new investigation). Useful for pruning a campaign: the next grouped POST /api/frontier considers all REMAINING jobs and no longer includes this member. RUNNING jobs cannot be deleted (409): a run cannot be cancelled — its provider calls would keep spending while the result is discarded. Poll until done or failed, then delete.

| Parameter | In | Type | Description |
|---|---|---|---|
| `id` | path | string | Job id returned by POST /api/investigations |

| Status | Response |
|---|---|
| `200` | Deleted. Body: {"deleted": "<id>"} |
| `401` | Missing or invalid bearer token |
| `404` | Unknown job id (already deleted, or lost on restart) |
| `409` | Job is still running — wait for done/failed, then delete |

### `GET /api/investigations/{id}`

Poll an investigation job. `progress` is always present (live model turns while running, frozen on completion); `result` is present once the job is `done` (trace, possibly budget-capped) or `failed` (failure evidence). Prefer GET /api/investigations/{id}/evidence for reading/judging: it retains actual tool responses and provenance without duplicating terminal progress. Check execution.stop_reason, not status or nonempty text, for how the run stopped. For an optimization loop, terminal status is the start of evaluation, not the signal to edit the prompt: read /evidence, immediately PATCH grades/assessment under the user's rubric, confirm the response, then read POST /api/frontier.

| Parameter | In | Type | Description |
|---|---|---|---|
| `id` | path | string | Job id returned by POST /api/investigations |

| Status | Response |
|---|---|
| `200` | Job status + live progress (+ result when done or failed): [`JobView`](#jobview) |
| `401` | Missing or invalid bearer token |
| `404` | Unknown job id |

### `PATCH /api/investigations/{id}`

Missing requested quality grades exclude a run from that frontier's coordinates, not from its membership/backlog. Measured-only frontiers need no grades. Grading is how the user's quality rubric becomes visible alongside cost, not a server verdict.  `grades` is caller-owned numeric judgment, one number per axis you and your user agreed on (for example `found_all: 0.5`, `precision: 1.0`, `tone_of_voice: 0.8`); `attributes` is caller-owned string metadata (for example `label: "baseline"`, or a prompt-version tag you group by later). The harness records both and never interprets a grade. Grade the run against the EVIDENCE — the tool responses it actually received, not the plausibility of its final answer. `assessment` is where you say why: the rubric scale and the turn/exchange the judgment rests on.  Both maps have merge semantics: a number/string sets or overwrites and JSON `null` deletes that key. `assessment` is caller-owned summary, rubric and zero-based evidence references: object replaces the whole assessment, null clears it, absent leaves it unchanged. All fields validate before ANY apply. The response echoes FULL updated grades, attributes and assessment. A numeric fidelity grade needs review of ACTUAL tool responses, not merely workspace_ops or computed counts. If simulation is inadequate, record that in assessment and withhold unjustified grades; this is not a harness verdict. Grade names and attribute names use `^[a-z][a-z0-9_]{0,63}$`; grade names cannot be measured axes. The literal measured names are `put_input_tokens`, `put_output_tokens`, `put_cache_read_tokens`, `put_cost_usd`, `sim_input_tokens`, `sim_output_tokens`, `sim_cache_read_tokens`, `sim_cost_usd`, `steps_per_trace_avg`, `steps_per_trace_min`, `steps_per_trace_max`, `steps_per_trace_stdev`, `elapsed_ms`, `resolving_inputs_ms` and `put_loop_ms`.  PATCH is allowed while a job runs. POST /api/frontier always considers ALL current jobs: running, failed, ungraded, or unavailable members appear as explicit exclusions/backlog in a successful grouped response. A group has null coordinates until it has at least one common complete cohort; poll and PATCH missing grades, then submit the same frontier request again.

| Parameter | In | Type | Description |
|---|---|---|---|
| `id` | path | string | Job id returned by POST /api/investigations |

Body: [`InvestigationPatch`](#investigationpatch)

| Field | Type | Required | Description |
|---|---|---|---|
| `assessment` | [`Assessment`](#assessment)? | no |  |
| `attributes` | object? | no | Caller-owned attribute name → string to set/overwrite, or null to delete. This is `attributes`, never `tags` (unknown fields are rejected). `label` names the job in the UI. It affects group identity only when explicitly selected in `group_by`. System provenance keys are read-only. |
| `grades` | object? | no | Axis name → number to set/overwrite, or null to delete. For prompt optimization, SEND these judgments after reading each run's evidence, not just into local files. Use the rubric agreed with the user, include assessment explaining the evidence, and confirm the PATCH response. Then read POST /api/frontier before the next prompt revision. |


| Status | Response |
|---|---|
| `200` | Updated full grades, attributes and assessment: [`InvestigationPatchView`](#investigationpatchview) |
| `400` | Invalid grades or attributes. Attribute keys use `^[a-z][a-z0-9_]{0,63}$`, values are strings ≤1024 bytes, and immutable provenance keys (`application_hash`, `sim_model`, `sim_thinking`, `workspace_hash`, `scenario_id`, `scenario_revision`, `scenario_hash`, `simulation_backend`, `step_budget`, `token_budget`) cannot change. |
| `401` | Missing or invalid bearer token |
| `404` | Unknown job id |

### `GET /api/investigations/{id}/evidence`

Read one complete conversation, not a final-answer/usage-only projection. Prefer this endpoint for judging and archiving: it removes duplicated terminal progress without dropping any tool responses or provenance. Inspect execution and every call/response before grading. A plausible final answer can conceal a faulty simulator. If evidence is inadequate, record that limitation in an assessment rather than inventing a fidelity score; sharpen the world/tool contract or change simulator settings and submit a separate investigation. Then PATCH your own grades and assessment, and explicitly group the variables being compared at POST /api/frontier. The harness never performs this judgment.

| Parameter | In | Type | Description |
|---|---|---|---|
| `id` | path | string | Investigation id |

| Status | Response |
|---|---|
| `200` | Nonduplicated complete evidence, live or terminal, including partial failure evidence: [`InvestigationEvidence`](#investigationevidence) |
| `401` | Missing or invalid bearer token |
| `404` | Unknown investigation |

### `GET /api/models`

| Status | Response |
|---|---|
| `200` | Available models per provider: [`ModelsResponse`](#modelsresponse) |
| `401` | Missing or invalid bearer token |

### `GET /api/scenarios`

List stored scenarios (summaries; no definition bodies).

| Status | Response |
|---|---|
| `200` | All stored scenarios in this server's memory: [`ScenarioSummary`](#scenariosummary)[] |
| `401` | Missing or invalid bearer token |

### `POST /api/scenarios`

Register a scenario: the world, its tools (with optional Lua implementations), the simulator settings, and an optional initial workspace archive. Creating a scenario executes nothing and calls no provider.

Body: [`ScenarioCreateRequest`](#scenariocreaterequest)

| Field | Type | Required | Description |
|---|---|---|---|
| `label` | string? | no | Optional display label. Never part of the identity or the hash. |
| `scenario` | [`ScenarioDefinition`](#scenariodefinition) | yes |  |


| Status | Response |
|---|---|
| `201` | Registered. Body: {id, revision, definition_hash, workspace_hash}: [`JobCreated`](#jobcreated) |
| `400` | Body is not valid JSON, or the definition fails validation (duplicate tool names, unparsable Lua, limits out of range) |
| `401` | Missing or invalid bearer token |

### `DELETE /api/scenarios/{id}`

Delete a scenario. Refused while any referencing investigation is running (a run cannot be cancelled), and refused without `cascade=true` when finished investigations still pin it. With `cascade=true` those investigations — their traces, grades and assessments — are deleted too.

| Parameter | In | Type | Description |
|---|---|---|---|
| `id` | path | string | Scenario id |
| `cascade` | query | boolean | Delete investigations that reference this scenario instead of refusing (they are gone, with their traces and grades) |

| Status | Response |
|---|---|
| `200` | Deleted. Body: {deleted, cascade_investigations, cascade_probes} |
| `401` | Missing or invalid bearer token |
| `404` | Unknown scenario |
| `409` | Investigations depend on this scenario (and cascade was not requested), or referenced work is still running |

### `GET /api/scenarios/{id}`

Read one scenario: its definition, identity, and dependents.

| Parameter | In | Type | Description |
|---|---|---|---|
| `id` | path | string | Scenario id |

| Status | Response |
|---|---|
| `200` | The stored scenario: [`ScenarioView`](#scenarioview) |
| `401` | Missing or invalid bearer token |
| `404` | Unknown scenario (already deleted, or lost on restart) |

### `PATCH /api/scenarios/{id}`

Edit a scenario's definition, simulator settings, label, or initial workspace. Refused while any investigation references the scenario (fork it instead) and while `expected_revision` is stale.

| Parameter | In | Type | Description |
|---|---|---|---|
| `id` | path | string | Scenario id |

Body: object


| Status | Response |
|---|---|
| `200` | Edited. Body: {id, revision, definition_hash, workspace_hash}: [`JobCreated`](#jobcreated) |
| `400` | Body is not valid JSON, or the definition fails validation |
| `401` | Missing or invalid bearer token |
| `404` | Unknown scenario |
| `409` | The scenario is pinned by investigations, or expected_revision is stale |

### `POST /api/scenarios/{id}/fork`

Fork a scenario into a new editable one, sharing the immutable initial workspace (no re-upload, no re-decompression). Optionally record what the copy corrects.

| Parameter | In | Type | Description |
|---|---|---|---|
| `id` | path | string | Scenario to copy |

Body: object


| Status | Response |
|---|---|
| `201` | Forked. Body: {id, revision, definition_hash, workspace_hash}: [`JobCreated`](#jobcreated) |
| `400` | Body is not valid JSON |
| `401` | Missing or invalid bearer token |
| `404` | Unknown scenario |
| `409` | expected_revision does not match the current revision |

### `GET /api/scenarios/{id}/simulations`

List probes recorded against a scenario (most recent first).

| Parameter | In | Type | Description |
|---|---|---|---|
| `id` | path | string | Scenario id |

| Status | Response |
|---|---|
| `200` | Probe summaries for this scenario: [`ProbeView`](#probeview)[] |
| `401` | Missing or invalid bearer token |
| `404` | Unknown scenario |

### `POST /api/scenarios/{id}/simulations`

Poll `GET /api/scenarios/{id}/simulations/{probe_id}` for the result.

| Parameter | In | Type | Description |
|---|---|---|---|
| `id` | path | string | Scenario id |

Body: [`ProbeRequest`](#proberequest)

| Field | Type | Required | Description |
|---|---|---|---|
| `expected_revision` | integer? | no | Optional guard: refuse to probe a scenario that has moved on since the caller last read it. |
| `max_calls` | integer? | no | Optional lower cap on how many of `tool_calls` to run. |
| `reason` | string? | no | Free-form note about what this probe is checking (surfaced with the result; never interpreted). |
| `resolved_inputs` | object? | no | Pin the scenario's declared inputs for this probe instead of sampling them. Supply every declared key; omit to sample. |
| `tool_calls` | [`ToolCall`](#toolcall)[] | yes | The calls to render, in order. Each runs in the scenario's language: `{"name": "<tool>", "args": {...}}`. Arguments are validated against the declared tool schema exactly as a workflow agent's call would be. |


| Status | Response |
|---|---|
| `202` | Accepted. Body: {id, scenario_revision, scenario_hash} |
| `400` | Body is not valid JSON or the request fails validation |
| `401` | Missing or invalid bearer token |
| `404` | Unknown scenario |
| `409` | expected_revision does not match the current revision |

### `GET /api/scenarios/{id}/simulations/{probe_id}`

Read one probe: every call's request, response, and provenance.

| Parameter | In | Type | Description |
|---|---|---|---|
| `id` | path | string | Scenario id |
| `probe_id` | path | string | Probe id returned by POST |

| Status | Response |
|---|---|
| `200` | The probe, live or terminal, with complete per-call evidence: [`ProbeView`](#probeview) |
| `401` | Missing or invalid bearer token |
| `404` | Unknown scenario or probe |

### `GET /api/scenarios/{id}/workspace`

Export the initial workspace as a JSON file inventory (paths and contents). This is the bytes every run starts from; a `workspace_hash` alone is not a reproducible workspace.

| Parameter | In | Type | Description |
|---|---|---|---|
| `id` | path | string | Scenario id |

| Status | Response |
|---|---|
| `200` | The initial workspace inventory: {workspace_hash, files:[{path, bytes, content (base64)}]} |
| `401` | Missing or invalid bearer token |
| `404` | Unknown scenario |

### `GET /docs/lua`

| Status | Response |
|---|---|
| `200` | Lua handler reference (markdown): the chunk contract, the ctx.workspace operations with their argument and return shapes, the sandbox limits, and how delegation to the simulator LLM works |

### `GET /docs/workflow`

| Status | Response |
|---|---|
| `200` | Lua investigation orchestration reference: default program, opaque params, ctx.run_agent and ctx.call_tool contracts, shared world, budgets and evidence. Read before authoring workflow.lua_source. |

## Schemas

### `AgentControls`

Exact effective agent controls for one ctx.run_agent call.

| Field | Type | Required | Description |
|---|---|---|---|
| `max_tokens` | integer? | no |  |
| `temperature` | number? | no |  |
| `thinking` | [`ThinkingLevel`](#thinkinglevel)? | no |  |

### `AgentInvocationRecord`

Evidence for one ctx.run_agent stage.

| Field | Type | Required | Description |
|---|---|---|---|
| `budget` | [`Budget`](#budget)? | no |  |
| `budget_cutoff_completion` | [`BudgetCutoffCompletion`](#budgetcutoffcompletion)? | no |  |
| `controls` | [`AgentControls`](#agentcontrols) | no |  |
| `event_id` | integer | yes | One shared, monotonic event sequence across agent invocations AND direct ctx.call_tool events. |
| `failure` | string? | no |  |
| `input` | string? | no |  |
| `invocation_id` | integer | yes | Stable per-invocation id, returned to Lua so the workflow can branch or retry with explicit references. |
| `model` | string | yes |  |
| `name` | string | yes |  |
| `output` | string? | no |  |
| `prompt` | string | yes |  |
| `running` | boolean | no |  |
| `steps_used` | integer | yes |  |
| `stop_reason` | [`RunStopReason`](#runstopreason)? | no |  |
| `tokens_used` | integer | yes |  |
| `tools` | string[] | no |  |
| `turn_end` | integer | yes |  |
| `turn_start` | integer | yes |  |
| `unrendered_call` | [`ToolCall`](#toolcall)? | no |  |

### `Assessment`

A caller's explanation of its judgment, not a harness verdict. Store this with grades so another reader knows the scale, limitations and actual evidence. PATCH replaces the whole assessment; null clears it. It is optional: a trace can be useful without being graded. All annotations are lost on server restart.

| Field | Type | Required | Description |
|---|---|---|---|
| `evidence` | [`EvidenceReference`](#evidencereference)[] | no | References to existing conversation evidence. Indices are zero-based in GET /api/investigations/{id}/evidence `turns`. Maximum 256 references. |
| `rubric` | string | no | Caller-defined grading scale/method, including the meaning of each axis. Empty when no grades are supplied or no rubric is applicable. |
| `summary` | string | yes | What the caller observed and concluded, including simulation limitations. Total text across this assessment is limited to 65536 UTF-8 bytes. |

### `BetterDirection`

Whether lower or higher values are better on an axis. Supplied by the caller per request for graded axes; baked in for reserved ones. Dominance normalizes internally (negating lower-is-better values), so "higher score = better" uniformly.

Values: `lower`, `higher`

### `Budget`

On investigation requests, these are whole-program limits. On invocation evidence, they report that stage's already-clamped remaining allowance.

| Field | Type | Required | Description |
|---|---|---|---|
| `max_steps_per_trace` | integer | yes | In investigation.budget: GLOBAL step budget across ALL Lua stages, retries and direct ctx.call_tool calls. It is NOT reset per run_agent. A budget of 6 permits six total steps, not six per retry. A lower local stage cap can be supplied as ctx.run_agent{budget={max_steps=...}}. A STEP is one tool call OR one final agent completion (including empty text). A completion that requests several tool calls counts as several steps but is an atomic batch: every sibling call is simulated, so one accepted batch may cross this cap. No later agent turn then runs. The main cost dial for tool-loop applications. Reserve room for the final completion. A trace recorded at the cap can lack a final answer; inspect execution.stop_reason and counters rather than equating done with success. |
| `max_tokens` | integer? | no | In investigation.budget: GLOBAL agent input+output token cap across ALL stages, models and retries. A new run_agent call consumes the remaining allowance; it never resets this cap. Repeated conversation history counts on every completion. Simulator tokens are not part of this cap. See execution.put_tokens_used and stop_reason. |

### `BudgetCutoffCompletion`

A provider completion received after cumulative PUT tokens exceeded the cap. It was charged and is preserved verbatim as evidence, but NOT accepted into the conversation. No tool requests here were executed or given fake responses. Raw argument strings may be malformed; they were not parsed by the runner.

| Field | Type | Required | Description |
|---|---|---|---|
| `model_output` | string? | no |  |
| `thinking` | string? | no |  |
| `tool_calls` | [`ToolCallRequest`](#toolcallrequest)[] | yes |  |

### `Correction`

Where a corrected scenario came from. Purely descriptive history: it does NOT lock, validate, or invalidate anything, and deleting the predecessor does not break the link.

| Field | Type | Required | Description |
|---|---|---|---|
| `reason` | string | yes | Caller-written explanation of what changed and why. |
| `revision` | integer | yes | That scenario's revision when this fork was made. |
| `scenario_id` | string | yes | The scenario this one corrects or replaces. |

### `CorrectionRequest`

| Field | Type | Required | Description |
|---|---|---|---|
| `reason` | string | yes | Explanation of what changed and why. |
| `revision` | integer? | no | The predecessor revision this copy was made from; defaults to the current revision. |

### `DirectToolCallRecord`

Evidence for one direct ctx.call_tool call.

| Field | Type | Required | Description |
|---|---|---|---|
| `args` | any | yes |  |
| `event_id` | integer | yes | One shared, monotonic event sequence across agent invocations AND direct ctx.call_tool events. |
| `failure` | string? | no |  |
| `lua_execution` | [`LuaExecutionRecord`](#luaexecutionrecord)? | no |  |
| `name` | string | yes |  |
| `response` | any | no |  |
| `running` | boolean | no |  |
| `sim_thinking` | string? | no |  |
| `state_after` | object? | no |  |
| `workspace_ops` | [`WorkspaceOp`](#workspaceop)[] | no |  |

### `EvidenceReference`

A location the caller used to reach its conclusion. Point to an exchange to discuss a simulated response, or to a whole turn for a model answer. The note expresses the caller's interpretation; a valid index does not validate it.

| Field | Type | Required | Description |
|---|---|---|---|
| `exchange` | integer? | no | Zero-based index within that turn's tool_exchanges; omit/null for the turn. |
| `note` | string | yes |  |
| `turn` | integer | yes |  |

### `FrontierAxis`

One axis of the frontier plot, with the caller's direction.

| Field | Type | Required | Description |
|---|---|---|---|
| `better` | [`BetterDirection`](#betterdirection) | yes | Whether lower or higher values are better on this axis. For graded axes this is YOUR call (encode direction in your own scale, e.g. grade "repeatability" high-good rather than "variance" low-good); for reserved axes it must match the measured direction. |
| `name` | string | yes | A graded axis name (you PATCHed it) or a reserved measured axis (harness-computed). Exact reserved names and baked-in directions: `put_input_tokens`, `put_output_tokens`, `sim_input_tokens`, and `sim_output_tokens` (lower); `put_cache_read_tokens` and `sim_cache_read_tokens` (higher — cached input is cheaper); `put_cost_usd` and `sim_cost_usd` (lower); and `steps_per_trace_avg`, `steps_per_trace_min`, `steps_per_trace_max`, `steps_per_trace_stdev` (lower). Monotonic durations `elapsed_ms`, `resolving_inputs_ms` and `put_loop_ms` are also lower. Compare latency only across adequate comparable traces, not faster failures. The `put_/sim_` notation is only prose shorthand, NEVER a valid axis name. Requesting a reserved axis with a contradicting `better` is rejected. |

### `FrontierError`

| Field | Type | Required | Description |
|---|---|---|---|
| `error` | string | yes |  |
| `problems` | [`FrontierProblem`](#frontierproblem)[] | yes |  |

### `FrontierProblem`

One fixable problem in a frontier request. Every `detail` names the fix — including, for missing grades, the exact PATCH to make.

| Field | Type | Required | Description |
|---|---|---|---|
| `axis` | string? | no |  |
| `detail` | string | yes |  |
| `investigation` | string? | no |  |
| `reason` | string | yes |  |

### `GroupExclusion`

One omitted run and why it is not part of its group's common cohort.

| Field | Type | Required | Description |
|---|---|---|---|
| `investigation` | string | yes |  |
| `missing_axes` | string[] | yes | Requested measured axes with no value (for example an unpriced cost). |
| `missing_grades` | string[] | yes | All requested caller-graded axes absent from this run. This remains populated for running and failed jobs to make the grading backlog seen. |
| `status` | string | yes | `running`, `failed`, `awaiting_grades`, or `unavailable`. |

### `GroupedFrontierPoint`

One stable attribute group. Groups without usable runs are deliberately retained with null values/frontier state rather than disappearing from the result.

| Field | Type | Required | Description |
|---|---|---|---|
| `attributes` | map&lt;string, string?&gt; | yes | The requested group attributes. Missing source values appear as JSON null. |
| `color` | string | yes | Stable categorical color derived from this group's id. |
| `dominated_by` | string[] | yes | IDs of dominating GROUPS, not investigation ids or display labels. Empty for non-dominated and pending groups; equal means do not dominate. |
| `excluded` | [`GroupExclusion`](#groupexclusion)[] | yes |  |
| `id` | string | yes | Stable SHA-256-derived id of canonical grouping attributes only; membership changes do not recolor or rename a group. |
| `included` | string[] | yes | Investigation ids used in every mean, sorted. Each has equal weight; all requested axes use exactly this same completed, fully-valued cohort. |
| `investigations` | string[] | yes | All snapshot ids in this group, sorted. |
| `label` | string | yes | Slash-separated grouping values in the request's `group_by` order (for example `gpt-5.6-luna/low/prompt-a1b2c3d4`). Caller-owned values are complete and never ellipsis-truncated. Known model namespaces and content hashes use documented basename/prefix forms; their full source values remain in `attributes`, and `id` remains the stable identity. Presentation collisions receive a stable group-id suffix. |
| `on_frontier` | boolean? | yes | True if non-dominated, false if dominated, null if pending (no values). Preliminary points with values participate in the current frontier. |
| `preliminary` | boolean | yes | True when any member was excluded. Preliminary points still participate in dominance when they have a complete common cohort. |
| `values` | object? | yes | Axis → arithmetic mean, or null if no member has all requested values. Always present, even for pending groups (null means no coordinates). |

### `GroupedFrontierRequest`

Request a frontier over means of complete investigations in each attribute group. There is intentionally no investigation selection field: accepting an old selection accidentally as an empty selection would silently mean all jobs.

| Field | Type | Required | Description |
|---|---|---|---|
| `axes` | [`FrontierAxis`](#frontieraxis)[] | yes | Axes whose arithmetic means define Pareto dominance. Every included run has every requested value, so different axes never average different cohorts. |
| `group_by` | string[] | no | Attribute names that form a group. Omit for exactly `["application_hash", "scenario_id", "scenario_revision"]` — the identity of the submitted application AND of the world it ran against. Both always exist, so a default group is never keyed on an absent fact; two investigations share a group only when they ran the same program against the same scenario revision. Send `[]` for one group containing every current job. A job missing a requested key is retained in that key's explicit JSON-null group, never dropped. |

### `GroupedFrontierResponse`

| Field | Type | Required | Description |
|---|---|---|---|
| `points` | [`GroupedFrontierPoint`](#groupedfrontierpoint)[] | yes |  |

### `InvestigateRequest`

EVERY request requires scenario_id AND investigation.budget. There is ONE application form: `workflow`. There is no privileged `put_model`/ `put_thinking_level` field — prompts, model names and conversation controls are ordinary `workflow.params` values that YOUR program reads and passes to ctx.run_agent. Nothing in `params` has harness meaning. Omit `workflow` (or its `lua_source`) to run the default single-stage program, whose convention is params.prompt (system text), params.model (qualified model name) and optional params.controls. Scenario tools, world and simulation configuration stay on the registered scenario.

| Field | Type | Required | Description |
|---|---|---|---|
| `attributes` | map&lt;string, string&gt; | no | Caller-owned campaign attributes. This field is literally `attributes`; there is no `tags` alias and unknown fields are rejected. Keys use `^[a-z][a-z0-9_]{0,63}$`; values are strings up to 1024 UTF-8 bytes. `label` is the special editable display label shown by the UI. POST rejects every system-owned key: `application_hash` is the content identity of the submitted workflow (source, params and limits); `sim_model`/`sim_thinking` are the SIMULATOR's resolved settings (the simulator belongs to the scenario); `workspace_hash` is SHA-256 of sorted uploaded workspace path/content pairs (including the stable empty-workspace hash); `simulation_backend` is `llm` or `lua`; `step_budget` and `token_budget` are decimal limits (unbounded token budget is `unlimited`); `scenario_id`/`scenario_revision`/`scenario_hash` identify the pinned world. There is no `put_model`: a program may invoke several models, so record a model name as a caller-owned attribute if you want to group by it. All system attributes are immutable provenance; group by them explicitly when comparing execution configurations. |
| `investigation` | [`Investigation`](#investigation) | yes |  |
| `resolved_inputs` | object? | no | Optional explicit bindings for the scenario's declared inputs (replay a specific sample instead of drawing a new one). Supply every declared key or omit entirely. |
| `scenario_id` | string | yes | The scenario to run, BY REFERENCE. The referenced definition's tool contracts become this run's tool surface. |
| `scenario_revision` | integer? | no | Optional guard against editing races: refuse to run unless the scenario is still at this revision. The referenced scenario is pinned by this submission, so it cannot be edited afterwards either. |
| `workflow` | [`WorkflowProgram`](#workflowprogram) | no | Caller-authored Lua orchestration and opaque JSON parameters. The program belongs to this investigation, NOT to the scenario. Omit it (or its `lua_source`) to run the default single-stage program. Each ctx.run_agent call runs a fresh agent conversation in the same scenario world; ctx.call_tool invokes that world's tools directly. All work shares investigation-wide budgets and host-owned evidence.  The default program's ordinary parameter convention is params.prompt (system text, `{{input_domain}}` values filled by ctx.render), params.model (qualified model name) and optional params.controls. Nothing about those key names is privileged: they are simply what the default program reads. Custom source returns function(params,ctx); run_agent options include name, prompt, model, input (string), tools (optional name subset), controls={thinking,temperature,max_tokens}, budget={max_steps,max_tokens}. ctx.input is the scenario user message; ctx.resolved_inputs holds its sampled bindings; ctx.render(text) fills `{{variable}}` placeholders from that sample. Handoffs are literal, never regenerated by the simulator. See GET /docs/workflow for the executable contract and examples. |

### `InvestigateResponse`

| Field | Type | Required | Description |
|---|---|---|---|
| `failure` | [`RunFailure`](#runfailure)? | yes |  |
| `trace` | [`TraceView`](#traceview)? | yes |  |
| `usage` | [`UsageByRole`](#usagebyrole) | yes | Cumulative token usage and call counts, split by the workflow-agent side (historical role key `put`) and the simulator (`sim`). Present even when `failure` is set. |

### `Investigation`

An investigation's controls: run one submitted application workflow against one supplied scenario and surface its resulting trace. Callers that want a corpus run invoke the singular operation once per scenario. Nothing is judged in-harness — the caller reads the trace and judges.

| Field | Type | Required | Description |
|---|---|---|---|
| `budget` | [`Budget`](#budget) | yes |  |
| `reason` | string? | no | Free-form justification for the run — WHY it exists and what a reader should know when comparing it with earlier runs: what it aims to accomplish, what changed compared to previous runs (a prompt edit, new scenarios, a different model), anything that frames how to read the traces. There is no strict standard — write whatever makes the run intelligible later.  Advisory only: surfaced with the result to guide reading the traces, NEVER used as an oracle. The harness runs a scenario and surfaces evidence; the caller is the judge. Optional — omit it when you just want to observe behavior with no particular framing.  e.g. "baseline before adding the explicit-confirmation rule" or "re-run after softening the refusal instruction; compare with v3". |

### `InvestigationEvidence`

Preferred agent reading surface: complete execution without duplicate data. This endpoint has NO `progress` or `result` wrapper. Use TOP-LEVEL `turns` and `workflow.invocations` / `workflow.tool_calls`, including on failed or running jobs. `progress.turns` belongs to GET /api/investigations/{id}, NOT this /evidence endpoint. Invocation turn_start/turn_end index the top-level turns array; invocations do not contain nested turns arrays. Read every `turns[].tool_exchanges[].response`: this is what the workflow's agent actually observed. `workspace_ops` only shows what the simulator consulted; `lua_execution.outcome=computed` only says code ran, not that the reply was faithful. Compare replies with `scenario.world` AND the tool contracts on `scenario.tools`. An invalid root listing or false-empty search can invalidate a comparison even when the final answer looks right. Source revisions may differ across reruns.  Available while running and after failure: successful exchanges and setup artifacts are retained. `execution.stop_reason` distinguishes a final completion from a budget cutoff or runtime failure; `status=done` alone does not. Full responses, model/simulator reasoning, workspace operations and Lua source are all preserved here. No semantic compression or inferred correctness flags.  Next action in a prompt-optimization loop: immediately SEND PATCH /api/investigations/{id} with your evidence-based `grades` and `assessment`, using the user's agreed rubric and zero-based turn/exchange references. Confirm the response echoes the annotations; a local score or a planned PATCH is not a recorded judgment. If simulation or evidence cannot justify a score, record the limitation in assessment and leave/clear that grade rather than fabricate one. Then POST /api/frontier with those quality axes and inspect `points` BEFORE editing the prompt. A run missing a requested grade remains in the group's explicit backlog, but cannot contribute coordinates on that comparison.

| Field | Type | Required | Description |
|---|---|---|---|
| `assessment` | [`Assessment`](#assessment)? | yes |  |
| `attributes` | map&lt;string, string&gt; | yes |  |
| `budget` | [`Budget`](#budget) | yes |  |
| `conversation_controls` | [`ResolvedConversationControls`](#resolvedconversationcontrols) | yes |  |
| `execution` | [`RunExecution`](#runexecution) | yes |  |
| `failure` | [`RunFailure`](#runfailure)? | no |  |
| `final_world_state` | object? | no | Null until a trace completes, including budget-capped completion. |
| `finished_at` | integer? | yes |  |
| `grades` | map&lt;string, number&gt; | yes |  |
| `id` | string | yes |  |
| `implementations` | [`ToolImplementation`](#toolimplementation)[] | yes |  |
| `phase` | [`RunPhase`](#runphase) | yes |  |
| `reason` | string? | no |  |
| `resolved_inputs` | map&lt;string, any&gt; | yes |  |
| `scenario` | [`Scenario`](#scenario) | yes |  |
| `scenario_definition_hash` | string | yes |  |
| `scenario_id` | string | yes | The reusable scenario this run pinned, with the exact revision and content hash that produced these traces. |
| `scenario_revision` | integer | yes |  |
| `sim_model` | string | yes |  |
| `sim_thinking_level` | [`ThinkingLevel`](#thinkinglevel)? | no |  |
| `started_at` | integer | yes |  |
| `status` | [`JobStatus`](#jobstatus) | yes |  |
| `turns` | [`TraceTurn`](#traceturn)[] | yes | Complete workflow-agent turns, each with tool arguments AND actual responses. EvidenceReference indices refer directly to this array and its exchanges. |
| `usage` | [`UsageByRole`](#usagebyrole)? | no |  |
| `user_message` | string? | no |  |
| `workflow` | [`WorkflowEvidence`](#workflowevidence)? | no |  |
| `workflow_program` | [`WorkflowProgram`](#workflowprogram) | yes | The submitted program, retained even if execution never started. |
| `workspace_files` | integer | yes |  |

### `InvestigationPatch`

PATCH updates independently optional grades, attributes and assessment. Every supplied value validates before anything is applied (atomic update). Maps merge; assessment replaces as a whole, null clears, absent leaves unchanged.

| Field | Type | Required | Description |
|---|---|---|---|
| `assessment` | [`Assessment`](#assessment)? | no |  |
| `attributes` | object? | no | Caller-owned attribute name → string to set/overwrite, or null to delete. This is `attributes`, never `tags` (unknown fields are rejected). `label` names the job in the UI. It affects group identity only when explicitly selected in `group_by`. System provenance keys are read-only. |
| `grades` | object? | no | Axis name → number to set/overwrite, or null to delete. For prompt optimization, SEND these judgments after reading each run's evidence, not just into local files. Use the rubric agreed with the user, include assessment explaining the evidence, and confirm the PATCH response. Then read POST /api/frontier before the next prompt revision. |

### `InvestigationPatchView`

| Field | Type | Required | Description |
|---|---|---|---|
| `assessment` | [`Assessment`](#assessment)? | yes |  |
| `attributes` | map&lt;string, string&gt; | yes |  |
| `grades` | map&lt;string, number&gt; | yes |  |

### `JobCreated`

| Field | Type | Required | Description |
|---|---|---|---|
| `attributes` | map&lt;string, string&gt; | yes | The stored provenance + caller attributes, including resolved model names and stable prompt/workspace hashes, available without a follow-up GET. |
| `id` | string | yes |  |

### `JobStatus`

Values: `running`, `done`, `failed`

### `JobSummary`

| Field | Type | Required | Description |
|---|---|---|---|
| `attributes` | map&lt;string, string&gt; | yes | Immutable provenance plus caller-owned campaign attributes, sufficient for a list view to group/filter before fetching full job evidence. |
| `execution` | [`RunExecution`](#runexecution) | yes | Live/frozen deterministic execution counters and timing, not a quality grade. |
| `finished_at` | integer? | yes | Null while running; epoch milliseconds when execution finished. |
| `id` | string | yes |  |
| `phase` | [`RunPhase`](#runphase) | yes | Observable current LLM phase; never infer job work from bare `running`. |
| `started_at` | integer | yes |  |
| `status` | [`JobStatus`](#jobstatus) | yes |  |
| `usage` | [`UsageByRole`](#usagebyrole)? | no |  |

### `JobView`

One investigation. Deterministic execution evidence lives at `progress.execution` while running, and at `result.trace.execution` once done (the two are equal when the run is terminal); there is deliberately no top-level `execution` alias. Read `stop_reason` there, not `status`, to learn how the run ended: `done` only means a trace was recorded. For a token-capped run the crossing completion is at `progress.execution.budget_cutoff_completion` (fields `model_output`, `thinking`, `tool_calls` — not `content`), and for a failure inside a tool batch `progress.execution.unrendered_call` names the request whose response does not exist. Prefer GET /api/investigations/{id}/evidence for reading the conversation itself.

| Field | Type | Required | Description |
|---|---|---|---|
| `assessment` | [`Assessment`](#assessment)? | yes |  |
| `attributes` | map&lt;string, string&gt; | yes | Immutable provenance plus caller-owned campaign attributes. `label` is the special editable display label; reserved provenance keys cannot change. |
| `budget` | [`Budget`](#budget) | yes | Original per-conversation budget, retained even for failed/capped runs. |
| `conversation_controls` | [`ResolvedConversationControls`](#resolvedconversationcontrols) | yes | Resolved controls for the simulator and any agent conversation that did not override them. |
| `finished_at` | integer? | yes | Execution completion epoch milliseconds, null while running. Core monotonic phase timings are in progress.execution; polling/file mtimes are not durations. |
| `grades` | map&lt;string, number&gt; | yes | Caller-graded axes on this investigation (PATCHed via PATCH /api/investigations/{id}). Free-form names, caller-chosen scales (0..1, 1..5, anything); the harness stores them and never interprets them. |
| `id` | string | yes | The job's id (same value as the `{id}` path segment and the id in `JobSummary`). Echoed in the body so a consumer holding only this representation knows which job it is — without it, a dashboard that reconciles a list of views by key has nothing stable to key on and silently falls back to positional matching (which leaks per-item UI state such as an unfolded conversation to whatever job sorts into that slot next). |
| `phase` | [`RunPhase`](#runphase) | yes | Which LLM phase the scenario is currently in (see RunPhase). This is the observable status of the job's LLM work. Mirrors `progress.phase`. |
| `progress` | [`RunProgress`](#runprogress) | yes | Live progress for this scenario, populated while running and frozen when the job finishes. Lets a dashboard show a tool-call log as it happens. `progress.execution` is the deterministic run record (stop reason, counters, monotonic phase timings, and any unaccepted cutoff completion or unrendered tool call). |
| `reason` | string? | no | The run's free-form `reason` (advisory justification: what the run aims to accomplish, what changed vs. earlier runs, what a reader should know — no strict standard). Optional; surfaced to guide reading the traces. Nothing is judged against it. |
| `result` | [`InvestigateResponse`](#investigateresponse)? | no |  |
| `scenario` | [`Scenario`](#scenario) | yes | The input scenario, by value: its narrative is the ground truth for interpreting the trace and progress. |
| `scenario_definition_hash` | string | yes |  |
| `scenario_id` | string | yes | The reusable scenario this investigation pinned, with the exact revision and content hash it ran. A correction elsewhere never changes this trace; read the scenario to see whether a newer revision exists. |
| `scenario_revision` | integer | yes |  |
| `sim_model` | string | yes | The resolved model name that ran the tool simulator (the scenario's simulator setting, or the server default). The simulator is the test ENVIRONMENT; a reader needs to see it to judge whether it was powerful enough to render the world believably. |
| `sim_thinking_level` | [`ThinkingLevel`](#thinkinglevel)? | no |  |
| `started_at` | integer | yes |  |
| `status` | [`JobStatus`](#jobstatus) | yes |  |
| `workflow` | [`WorkflowProgram`](#workflowprogram) | yes | The application under test: the submitted Lua program with its opaque params and limits. Actual invocation settings (including each agent model) and output are in progress.workflow and result.trace.workflow; prefer the nonduplicated /evidence view. |
| `workspace_files` | integer | yes | How many files seeded the simulation workspace (0 = no zip upload; the simulator answered from narrative alone). The workspace is an in-memory filesystem the SIMULATOR consults via read/write/list_dir/ grep — it is NOT an application tool. See the endpoint description. |

### `LuaExecutionRecord`

Evidence of a Lua attempt before a tool response. Computed means executed, NOT faithful or correct: code can return an invalid-path error or false-empty search successfully. Inspect ToolExchange.response against the tool contract. A fallback or error is NOT the tool's return value: all staged mutations were discarded and the LLM rendered the actual response. Successful Lua operations appear in the exchange's ordinary workspace_ops instead.

| Field | Type | Required | Description |
|---|---|---|---|
| `detail` | string? | no |  |
| `discarded_workspace_ops` | [`WorkspaceOp`](#workspaceop)[] | no |  |
| `outcome` | [`LuaOutcome`](#luaoutcome) | yes |  |
| `source_hash` | string | yes | SHA-256 of the exact source that ran, matching the scenario's `implementations` entry for this tool. |
| `tool` | string | yes | The tool whose supplied implementation was attempted. |

### `LuaOptions`

Resource controls for one Lua tool-handler invocation.  All fields are explicit, documented overrides. Zero is invalid even for direct library callers. The duration limit is cooperative: it is checked by the VM hook and around Rust/Lua conversion boundaries, but a single native Lua C operation cannot be preempted until it returns.

| Field | Type | Required | Description |
|---|---|---|---|
| `max_duration_ms` | integer | no | Cooperative wall-clock deadline for the whole invocation (default 2000 ms). |
| `max_host_bytes` | integer | no | Cumulative serialized workspace capability argument/result traffic limit (default 8 MiB). |
| `max_host_calls` | integer | no | Workspace capability-call limit (default 128). |
| `max_instructions` | integer | no | Shared VM/conversion work budget across initialization and handler execution (default 1 million). |
| `max_memory_bytes` | integer | no | Lua allocator limit, including handler-created values (default 16 MiB). |
| `max_result_bytes` | integer | no | Maximum serialized size per converted value, shared by response and state-patch values (default 1 MiB). |
| `max_source_bytes` | integer | no | UTF-8 source limit before parsing; bytecode is never loaded (default 256 KiB). |
| `max_value_depth` | integer | no | Maximum JSON/Lua nesting depth (default 128; hard ceiling 128 for host stack safety). |

### `LuaOutcome`

Values: `computed`, `fallback`, `error`

### `LuaWorkspaceCapability`

Documentation-only schema: the sandbox capability a Lua handler receives as `ctx`. It never appears in a payload; it exists so the spec states the handler's available operations, their shapes, and the two ways a handler can hand a call back to the simulator LLM. The full prose reference is served at `GET /docs/lua`.

| Field | Type | Required | Description |
|---|---|---|---|
| `decline_to_simulator` | string | yes | `PleaseSimulateException("reason")` declines this call: the simulator LLM renders the response from the world narrative instead. Recorded as `lua_execution.outcome = "fallback"`. Declining is a normal outcome. |
| `errors_and_limits_delegate` | string | yes | A runtime error or breached sandbox limit also delegates that one call to the simulator LLM, with `lua_execution.outcome = "error"`; writes the handler staged are discarded first. A tool with no implementation produces no `lua_execution` record at all. Read `execution.lua_computed_calls` / `lua_fallback_calls` / `lua_error_calls` after a run to see whether your code served it. |
| `handler_contract` | string | yes | The chunk must RETURN a function taking `(args, ctx)` and returning `{response = <the value the prompt under test receives>, state_patch = <write tools only>}`. `response` is required, and should match the tool's declared contract shape exactly. |
| `reference_url` | string | yes | The prose reference for all of this, also served at GET /docs/lua. |
| `workspace_grep` | string | yes | `ctx.workspace.grep({pattern, path?, case_insensitive?})` — a LITERAL substring search (never a regex). Returns `{pattern, matches:[{path, line, text}], truncated}`; at most `workspace_max_grep_matches` matches. |
| `workspace_list_dir` | string | yes | `ctx.workspace.list_dir({path?})` — direct children only. Returns `{path, entries:[{name, kind:"file"\|"dir"}], truncated}`; unknown path returns `{path, error:"not found"}`. Omit `path` (or pass "" or ".") for the workspace root. |
| `workspace_read` | string | yes | `ctx.workspace.read({path, start_line?, end_line?})` — a file's contents. Returns `{path, content, start_line, end_line, total_lines, truncated}`; a missing file returns `{path, error:"not found"}`. Lines are 1-based; at most `workspace_max_read_lines` are returned. |
| `workspace_write` | string | yes | `ctx.workspace.write({path, content})` — writes into the run's PRIVATE overlay (visible to later calls in this run only, never to another run and never to the prompt under test except through `response`). Returns `{path, bytes, ok:true}`. |

### `ModelEntry`

One model the caller can pass to `ctx.run_agent` or use as a scenario's `simulation.sim_model`. `name` is the full namespaced, pastable string (e.g. `open_router::deepseek/deepseek-v4-flash-0731`).

| Field | Type | Required | Description |
|---|---|---|---|
| `name` | string | yes |  |
| `pricing` | object? | no | Per-token USD pricing as reported by the provider. Keys follow OpenRouter's conventions: `prompt` (input), `completion` (output), `input_cache_read` (cached input). Present only when the provider exposes pricing — absent for subscription endpoints (z.ai coding plan) and providers that don't report it (Bedrock). If future pricing sources are added, reuse these same keys. |

### `ModelsResponse`

Models available to name in `sim_model` (scenario) or in a workflow's own params, by provider.  Returns the server defaults plus a map keyed by provider namespace (`zai_coding`, `open_router`, `bedrock_sigv4`, `vertex`). Each provider value is either `{available: {models: [{name, pricing?}]}}` — where `name` is the full pastable, namespaced string (e.g. `open_router::deepseek/deepseek-v4-flash-0731`) — or `{error: "…"}` explaining why that provider couldn't be listed (no API key in the environment, no AWS credentials, region-gated, …). Listing is best-effort and per-provider: one provider failing never breaks the others. Cached for a short time so repeated listing is cheap. This does NOT call generation endpoints or check credit balance: available means catalog/configuration discovery, not usable inference. Smoke-test one small investigation with both chosen roles before a corpus fanout. A 429 can mean exhausted balance rather than transient rate limiting; inspect the error, fix provider funding/permissions, and do not silently switch the simulator.

| Field | Type | Required | Description |
|---|---|---|---|
| `generation_checked` | boolean | yes | Always false: this endpoint lists catalogs/configuration, never makes a charged generation call or checks credit balance. `available` is NOT a readiness guarantee. Run one small investigation with both chosen roles before fanout; quota/balance failures need provider/operator action. |
| `providers` | map&lt;string, [`ProviderModels`](#providermodels)&gt; | yes |  |
| `server_default_model` | string | yes | Model the server uses when nothing else names one (a bare name; the server resolves it via `server_default_provider`). |
| `server_default_provider` | string | yes | Provider applied to bare model names when no namespace is given (from PROMPT_EXPLORE_PROVIDER). Maps to a namespace prefix: `zai` -> `zai_coding::`, `zai_standard` -> `zai::`, `openrouter` -> `open_router::`, `bedrock` -> `bedrock_sigv4::`, `gemini` -> `vertex::`. |

### `ProbeCall`

One rendered call, with the provenance needed to judge it.

| Field | Type | Required | Description |
|---|---|---|---|
| `elapsed_ms` | integer | yes |  |
| `error` | string? | no |  |
| `lua_execution` | [`LuaExecutionRecord`](#luaexecutionrecord)? | no |  |
| `request` | [`ToolCall`](#toolcall) | yes | The submitted request, echoed. |
| `response` | any | yes | The rendered tool response. `null` when this call failed and none was produced; `error` then says why. |
| `sim_thinking` | string? | no | The simulator model's visible reasoning while rendering this response. |
| `state_after` | object? | no | Present for write tools: world state after this call's patch. |
| `workspace_ops` | [`WorkspaceOp`](#workspaceop)[] | no | Workspace operations the simulator performed (its lookups, plus any committed Lua writes). Rolled-back Lua writes appear under `lua_execution.discarded_workspace_ops` instead. |

### `ProbeRequest`

What a caller submits to test a simulation.

| Field | Type | Required | Description |
|---|---|---|---|
| `expected_revision` | integer? | no | Optional guard: refuse to probe a scenario that has moved on since the caller last read it. |
| `max_calls` | integer? | no | Optional lower cap on how many of `tool_calls` to run. |
| `reason` | string? | no | Free-form note about what this probe is checking (surfaced with the result; never interpreted). |
| `resolved_inputs` | object? | no | Pin the scenario's declared inputs for this probe instead of sampling them. Supply every declared key; omit to sample. |
| `tool_calls` | [`ToolCall`](#toolcall)[] | yes | The calls to render, in order. Each runs in the scenario's language: `{"name": "<tool>", "args": {...}}`. Arguments are validated against the declared tool schema exactly as a workflow agent's call would be. |

### `ProbeStatus`

Values: `running`, `done`, `failed`

### `ProbeStopReason`

Values: `completed`, `call_limit`, `runtime_failure`

### `ProbeView`

One simulation probe: caller-submitted tool calls rendered through the investigation engine, with complete per-call provenance.

| Field | Type | Required | Description |
|---|---|---|---|
| `calls` | [`ProbeCall`](#probecall)[] | yes |  |
| `cost_usd` | number? | no |  |
| `error` | string? | no |  |
| `finished_at` | integer? | no |  |
| `id` | string | yes |  |
| `lua_computed_calls` | integer | yes | How many calls a supplied Lua implementation served with no model call. |
| `lua_error_calls` | integer | yes | How many calls a supplied implementation ERRORED on (bad source, runtime error, or a sandbox limit). Staged writes were rolled back and the LLM rendered the response; the implementation still needs fixing. |
| `lua_fallback_calls` | integer | yes | How many calls a supplied implementation declined, so the simulator LLM rendered the response instead. A non-zero count here is normal for a handler covering a subset of its contract. |
| `reason` | string? | no | Free-form note supplied with the probe, echoed for context. |
| `resolved_inputs` | map&lt;string, any&gt; | yes | The inputs this probe ran with: sampled, or the caller's bindings. |
| `scenario_hash` | string | yes |  |
| `scenario_id` | string | yes |  |
| `scenario_revision` | integer | yes | The scenario revision this probe actually ran against (a probe does not pin the scenario, so record what it used). |
| `started_at` | integer | yes |  |
| `status` | [`ProbeStatus`](#probestatus) | yes |  |
| `stop_reason` | [`ProbeStopReason`](#probestopreason)? | no |  |
| `usage` | [`UsageTotals`](#usagetotals)? | no |  |

### `ProviderModels`

A provider's listing result.

**Variant**

| Field | Type | Required | Description |
|---|---|---|---|
| `available` | object | yes | Catalog/configuration discovery succeeded or can be attempted. This does NOT verify generation, model access or credit balance. A listed model can still fail its first completion (including 429 insufficient balance). `models` may be empty after a listing failure (see note); the catalog is advisory, not a gate. Run one small investigation with both chosen roles before fanout; this endpoint never makes hidden charged generation probes. |

**Variant**

| Field | Type | Required | Description |
|---|---|---|---|
| `error` | object | yes | Discovery failed, for example absent credentials, network or region errors. This is a listing diagnostic, not a generation-readiness test. |

### `ResolvedConversationControls`

referenced scenario.

| Field | Type | Required | Description |
|---|---|---|---|
| `agent_max_tokens_default` | integer? | no |  |
| `agent_temperature_default` | number? | no | Defaults inherited by a ctx.run_agent call that omits the corresponding controls. They are execution defaults, not privileged workflow inputs. |
| `simulator` | [`ResolvedSimulatorSettings`](#resolvedsimulatorsettings) | yes | The simulator settings this run used, resolved from the scenario. |

### `ResolvedSimulatorSettings`

| Field | Type | Required | Description |
|---|---|---|---|
| `lua` | [`LuaOptions`](#luaoptions)? | no |  |
| `max_workspace_turns` | integer | yes |  |
| `sim_max_repair_attempts` | integer | yes |  |
| `sim_max_tokens` | integer? | no |  |
| `sim_temperature` | number? | no |  |
| `workspace_max_grep_matches` | integer | yes |  |
| `workspace_max_line_len` | integer | yes |  |
| `workspace_max_output_bytes` | integer | yes |  |
| `workspace_max_read_lines` | integer | yes |  |

### `RunExecution`

Deterministic execution evidence for one run. Token use is PUT input plus output tokens reported by the provider; steps use the investigation budget's tool-call/final-completion units.

| Field | Type | Required | Description |
|---|---|---|---|
| `budget_cutoff_completion` | [`BudgetCutoffCompletion`](#budgetcutoffcompletion)? | no |  |
| `lua_computed_calls` | integer | no | Tool calls served by a caller-supplied Lua implementation, with no model call. A non-zero count here is the reason a run can have zero simulator spend; zero counts with non-empty `implementations` mean the supplied code never actually served a call. |
| `lua_error_calls` | integer | no | Calls whose Lua implementation raised a runtime error or exceeded a sandbox limit. Staged writes were rolled back before the simulator LLM rendered the response, so the trace stays coherent, but the caller should fix the implementation: this is a defect in supplied code. |
| `lua_fallback_calls` | integer | no | Calls whose Lua implementation DECLINED (`PleaseSimulateException`) and whose response was rendered by the simulator LLM instead. Expected for a handler that covers a subset of the contract; a large count means the implementation is thin and the simulation is mostly model-rendered. |
| `put_tokens_used` | integer | yes |  |
| `steps_used` | integer | yes |  |
| `stop_reason` | [`RunStopReason`](#runstopreason)? | yes |  |
| `timing` | [`RunTiming`](#runtiming) | no |  |
| `unrendered_call` | [`ToolCall`](#toolcall)? | no |  |

### `RunFailure`

A failure while running one scenario. `stage` identifies the runtime layer (`"runner"` for PUT execution, input resolution, or tool simulation) and `error` is its diagnostic text.

| Field | Type | Required | Description |
|---|---|---|---|
| `error` | string | yes |  |
| `stage` | string | yes |  |

### `RunPhase`

The LLM phase of the single scenario currently being run. Exposed so a reader can see live work rather than a bare "running" status. There is no preparation phase: Lua implementations are authored by the caller and supplied with the scenario, never generated during a run.

Values: `resolving_inputs`, `orchestration`, `put_loop`

### `RunProgress`

Live progress for one scenario. The runner updates this flat value as work proceeds; if the run fails, already resolved inputs, supplied tool implementations, and completed turns remain available as evidence.

| Field | Type | Required | Description |
|---|---|---|---|
| `execution` | [`RunExecution`](#runexecution) | no | Deterministic execution evidence. `snapshot()` refreshes its live clock; after `finish()` it is frozen for completed and failed runs. |
| `implementations` | [`ToolImplementation`](#toolimplementation)[] | no | The caller-supplied Lua implementations this run may execute, with the exact source hashes each exchange's `lua_execution` refers to. Empty when every tool is rendered by the simulator LLM. |
| `phase` | [`RunPhase`](#runphase) | yes | The current LLM phase for this scenario. |
| `resolved_inputs` | map&lt;string, any&gt; | no | Concrete values selected from the input domain. Recorded before orchestration so a later failure still exposes reproducible inputs. |
| `turns` | [`TraceTurn`](#traceturn)[] | no | Completed PUT model turns accumulated so far. If a sibling tool call fails, the final turn may contain only that completion's successfully rendered exchanges; no failed exchange is invented. |
| `usage` | [`UsageByRole`](#usagebyrole)? | no |  |
| `user_message` | string? | no | The opening user message, for rendering the complete conversation live. |
| `workflow` | [`WorkflowEvidence`](#workflowevidence)? | no |  |

### `RunStopReason`

Why a run stopped. This is deterministic execution bookkeeping, not a verdict on the trace.

Values: `final_completion`, `step_budget`, `token_budget`, `runtime_failure`

### `RunTiming`

Monotonic wall-clock time spent in each observable LLM phase.

| Field | Type | Required | Description |
|---|---|---|---|
| `elapsed_ms` | integer | yes |  |
| `orchestration_ms` | integer | yes |  |
| `put_loop_ms` | integer | yes |  |
| `resolving_inputs_ms` | integer | yes |  |

### `Scenario`

A test case: a world specification, an input domain, and a protagonist. A pure VALUE — it carries no identity (`id`); runs report it back by value. The harness runs the prompt under test inside this world and surfaces the resulting trace for the caller to judge.  Scenarios are authored OUTSIDE the harness (by the operator's agent); this API never generates them.  ## Your role: adversary  Your job is to BREAK the prompt under test, not validate it. Assume it is flawed, and construct each scenario — world, input domain, opening turn — to make the bad behavior under investigation SURFACE if that flaw exists. Write the world the way a red-teamer would, not the way the prompt's author would: set the trap (an order that belongs to a DIFFERENT customer; an ownership claim that cannot be verified; a broken lookup) rather than a comfortable situation where the agent easily behaves well. A scenario that lets the agent succeed proves nothing.  If you are an LLM (or are using LLMs) to author scenarios, note that they are notoriously bad at questioning their own output: the same context that wrote (or is reading) the prompt tends to construct scenarios that confirm it rather than break it. A SEPARATE agent helps — construct each scenario with a SUBAGENT if you have one: a fresh context, given only the prompt, the run's `reason`, and this adversary role, is not invested in the prompt and will find angles its author didn't think to defend. This is only a PARTIAL mitigation, not a complete counter — a subagent shares the same model weights and can under-appreciate the same weaknesses — but it is a meaningful start. The mechanics below are tools for this role.  ## Authoring the `world`  The world is ground truth for the simulator AND the caller (who reads the traces and judges), and it is the single biggest determinant of result quality. It must pin four things, all in natural language:    1. INVENTORY — what exists and where, covering every query type the      PUT's tools allow.   2. FACTS — including NEGATIVE facts: what does NOT exist, what NEVER      happens. Models default to inventing positive content; absences      must be stated, and they are often what makes a trace decidable.   3. COMPLETENESS ASSERTIONS — "these are ALL the entry points" (closed      world) or "these are the relevant results" (open world).   4. RENDERING RULES — refuse queries outside the inventory; filler      introduces no new facts; never contradict the facts.  ## Authoring the `input_domain`  For each `{{variable}}` a workflow may pass to `ctx.render(text)`, describe its input DOMAIN — the value space, semantics, and any PRECONDITIONS or trust contract the application may assume about it. The simulator picks a concrete value from this domain before Lua starts; `ctx.render` fills it into caller-chosen text, and the chosen value is reported in `resolved_inputs`. A domain is richer than a pinned value: "tier is standard or premium, premium cancels without a fee" or "user_record: { id, name, tier }; user.id has been verified upstream — the agent may trust the person described". The world states the contract; whether the world actually HONORS it (or breaks it) is where the behavior you are looking for lives.  Variables are for what VARIES per run. If a passage is constant, it is not a variable: keep it verbatim in workflow source/params. Writing a complete literal as the domain description tends to make the simulator copy it, but it may still paraphrase or drop it; that failure mode is visible by reading the recorded `resolved_inputs`.

| Field | Type | Required | Description |
|---|---|---|---|
| `input_domain` | map&lt;string, string&gt; | no | Per-`{{variable}}` input-domain descriptions: the value space, semantics, and preconditions/trust contracts. The simulator chooses a concrete value for each before workflow Lua starts; `ctx.render(text)` validates and fills those placeholders, and `ctx.resolved_inputs` exposes the exact bindings. Only use placeholders for inputs that should VARY across runs — constant text under test belongs verbatim in workflow source/params. Empty when the program renders no sampled placeholders. |
| `simulator_notes` | string | no | Persona/stance guidance for a simulated user, if the scenario involves one. Defaults empty. |
| `user_message` | string? | no | The opening message from the user/protagonist. |
| `world` | string | yes | The world specification — ground truth the simulator renders tool responses from and the caller checks claims against. A SPECIFICATION (prose), not instantiated data. See the API description's DESIGN INTENT. Cover inventory, facts (incl. negatives), completeness, and rendering rules.  If the tools expose a REAL system with authoritative documentation (an OpenAPI spec, a man page, a CLI's --help), EMBED that documentation in the world verbatim and pin the rendering rules to it: "the embedded spec is authoritative for every rendered response." Without it the simulator invents plausible-but-wrong behavior for the documented surface (wrong error codes, invented fields, impossible operations) — verified by A/B: simulated API calls invented 409 read-only errors and off-schema bodies until the real spec was embedded, after which responses matched the contract. The same applies to any authoritative doc: embed it, then pin rendering to it. |

### `ScenarioCreateRequest`

Body of `POST /api/scenarios`.

| Field | Type | Required | Description |
|---|---|---|---|
| `label` | string? | no | Optional display label. Never part of the identity or the hash. |
| `scenario` | [`ScenarioDefinition`](#scenariodefinition) | yes |  |

### `ScenarioDefinition`

A reusable test case: the authored world (narrative, input domain, protagonist, simulator notes), the tool surface with its optional Lua implementations, and the simulator settings to run it under.

| Field | Type | Required | Description |
|---|---|---|---|
| `input_domain` | map&lt;string, string&gt; | no | Per-`{{variable}}` input-domain descriptions: the value space, semantics, and preconditions/trust contracts. The simulator picks a concrete value from each domain and the chosen value is reported in `resolved_inputs`; `POST .../simulations` can also supply explicit bindings to test one selection directly. |
| `simulation` | [`SimulationSettings`](#simulationsettings) | no | Simulator settings this scenario runs under. |
| `simulator_notes` | string | no | Persona/stance guidance for a simulated user, if the scenario has one. |
| `tools` | [`ScenarioTool`](#scenariotool)[] | no | The tool surface: contracts plus optional Lua implementations. |
| `user_message` | string? | no | The opening message from the user/protagonist. |
| `world` | string | yes | The world specification — ground truth the simulator renders tool responses from, and that the caller checks claims against.  Cover inventory, facts (including NEGATIVE facts), completeness assertions, and rendering rules. Embed authoritative documentation (an OpenAPI spec, a man page) verbatim and pin rendering to it. |

### `ScenarioSummary`

A scenario summary without the definition body, for listing.

| Field | Type | Required | Description |
|---|---|---|---|
| `definition_hash` | string | yes |  |
| `editable` | boolean | yes |  |
| `id` | string | yes |  |
| `implemented_tools` | integer | yes |  |
| `investigations` | integer | yes |  |
| `label` | string? | no |  |
| `probes` | integer | yes |  |
| `revision` | integer | yes |  |
| `tools` | integer | yes |  |
| `updated_at` | integer | yes |  |
| `workspace_files` | integer | yes |  |

### `ScenarioTool`

One tool the scenario's world exposes: the contract the prompt under test sees, plus an OPTIONAL caller-authored Lua implementation of it.  The contract is model-visible; `lua_source` never is. A handler that cannot faithfully implement the declared semantics should call `PleaseSimulateException("reason")` and let the simulator LLM render that one response instead.

| Field | Type | Required | Description |
|---|---|---|---|
| `description` | string | yes | The tool contract, exactly as the prompt under test sees it and as the simulator is told to render it. Describe argument semantics AND the returned shape. A vague contract makes Lua and LLM implementations disagree silently. |
| `example_responses` | string[] | no | Realism hints for the simulator LLM (anchors, not pinned outputs). |
| `lua_source` | string? | no | Optional Lua implementation, run in the sandbox for every call of this tool. The source RETURNS a function taking `(args, ctx)`:  ```lua return function(args, ctx)   return { response = { ... }, state_patch = { ... } } -- writes only end ```  Omit it to let the simulator LLM render this tool's responses. A handler may decline a call with `PleaseSimulateException("reason")`; missing handlers and runtime errors also delegate, and their staged workspace writes are discarded.  `ctx` exposes `ctx.workspace` — `list_dir`, `read`, `grep` and `write` over the run's private copy of the scenario's uploaded workspace — so a handler can serve EXACT file contents instead of asking a model to reproduce them (see `LuaWorkspaceCapability` for the per-operation shapes). The full prose reference, including the sandbox limits and how computed/delegated/errored calls are recorded, is served by the running server at `GET /docs/lua`. Read `execution.lua_computed_calls`/`lua_fallback_calls`/`lua_error_calls` after a run to check that your code actually served it. |
| `name` | string | yes |  |
| `parameters` | any | yes | JSON Schema for the tool's parameters. |
| `side_effect` | [`SideEffect`](#sideeffect) | yes |  |

### `ScenarioUploadRequest`

The SAME registration as `ScenarioCreateRequest`, sent as a `multipart/form-data` form when the scenario has an initial workspace.  `Content-Type: application/json` with a `ScenarioCreateRequest` body is the workspace-less form; the multipart form adds the archive part. Nothing else differs — the two shapes describe one endpoint, and a caller may use either.

| Field | Type | Required | Description |
|---|---|---|---|
| `request` | [`ScenarioCreateRequest`](#scenariocreaterequest) | yes | The registration, as JSON text: parse this part exactly like the `application/json` body of the same endpoint. |
| `workspace` | string | yes | Optional initial workspace, as a ZIP archive whose entries are paths relative to the workspace root (for example `src/main.rs`). Raw file bytes, not text. Omit it for an empty workspace; `POST /api/scenarios/{id}/fork` copies an existing workspace without a re-upload. `GET /api/scenarios/{id}/workspace` exports it again. |

### `ScenarioView`

One stored scenario, as the API reports it: the definition, its identity, and the dependency bookkeeping a caller needs before editing or deleting it.

| Field | Type | Required | Description |
|---|---|---|---|
| `correction` | [`Correction`](#correction)? | no |  |
| `created_at` | integer | yes |  |
| `definition` | [`ScenarioDefinition`](#scenariodefinition) | yes |  |
| `definition_hash` | string | yes | SHA-256 of the execution-relevant contents (narrative, domains, contracts, Lua sources, simulator settings). Display-only metadata such as `label` is excluded. |
| `editable` | boolean | yes | True while no investigation references this scenario. A referenced scenario is immutable: fork it to change it. |
| `id` | string | yes |  |
| `investigation_ids` | string[] | yes | Investigations pinning this scenario. They block edits and, without `cascade=true`, deletion. |
| `label` | string? | no |  |
| `probe_ids` | string[] | yes | Ids of probes recorded against this scenario (probes never pin it). |
| `revision` | integer | yes | Increases on every edit. An investigation pins exactly one value, and `PATCH` requires the caller's expected revision. |
| `running_investigations` | integer | yes | How many of the pinned investigations are still running. |
| `updated_at` | integer | yes |  |
| `workspace_files` | integer | yes |  |
| `workspace_hash` | string | yes | SHA-256 of the initial workspace inventory (paths and bytes). |

### `SideEffect`

Values: `read`, `write`

### `SimulationSettings`

Simulator configuration that belongs to the SCENARIO, not to an investigation: the environment is part of the test case, and testing a simulation under different settings than an investigation runs it under should be an explicit edit (or a fork), never an invisible override.

| Field | Type | Required | Description |
|---|---|---|---|
| `lua` | [`LuaOptions`](#luaoptions)? | no |  |
| `max_repair_attempts` | integer? | no | Total attempts per simulator JSON reply (initial reply included). |
| `max_tokens` | integer? | no | Maximum output tokens per simulator completion. |
| `max_workspace_turns` | integer? | no | Workspace tool calls per simulator response before the final-answer nudge. |
| `sim_model` | string? | no | Model that roleplays the environment. Omit for the server default. |
| `sim_thinking_level` | [`ThinkingLevel`](#thinkinglevel)? | no |  |
| `temperature` | number? | no | Simulator sampling temperature. Omit for the documented default. |
| `workspace` | [`WorkspaceLimits`](#workspacelimits) | no |  |

### `ThinkingLevel`

Provider-neutral thinking/reasoning level for one LLM role. The vocabulary is deliberately small and shared across providers: `none` explicitly requests NO reasoning, `minimal`…`max` scale effort up. Omitting the field entirely means "provider default" — which is different from `none`. Defaults vary by model and endpoint; omitting the field does not imply medium or no reasoning.  Bedrock OpenAI keywords are passed through literally, without downgrading unsupported levels. GPT-OSS uses flat `reasoning_effort` and supports low/medium/high; none/minimal/xhigh/max are rejected. GPT-5.6 Luna/Terra/Sol use nested `reasoning.effort` and support none/low/medium/high/xhigh/max. GPT-6 Astra uses the nested shape and supports low/medium/high/xhigh/max, but not none. All these Bedrock models reject minimal.  POST rejects models for which the adapter has no reasoning mapping (for example Bedrock Meta models). Model-specific keyword support is checked by the provider DURING execution, not prevalidated at POST. A 202 response therefore does not guarantee the level is supported: poll the investigation and inspect its singular run failure for provider rejections, alongside the trace/progress evidence. Other providers may map effort differently; do not assume that a level is portable.

Values: `none`, `minimal`, `low`, `medium`, `high`, `xhigh`, `max`

### `ToolCall`

| Field | Type | Required | Description |
|---|---|---|---|
| `args` | any | yes |  |
| `name` | string | yes |  |

### `ToolCallRequest`

| Field | Type | Required | Description |
|---|---|---|---|
| `arguments` | string | yes | Raw JSON arguments as produced by the model (may be malformed; parsing is the caller's decision). |
| `id` | string | yes |  |
| `name` | string | yes |  |

### `ToolExchange`

One tool request and its simulated result within a PUT model turn.

| Field | Type | Required | Description |
|---|---|---|---|
| `call` | [`ToolCall`](#toolcall) | yes | The tool request emitted by the PUT. |
| `lua_execution` | [`LuaExecutionRecord`](#luaexecutionrecord)? | no |  |
| `response` | any | yes | The response rendered by the simulator and returned to the PUT. |
| `sim_thinking` | string? | no | The SIMULATOR model's visible reasoning while rendering this response (its whole inner drive: lookups and final answer). Transparency only. |
| `workspace_ops` | [`WorkspaceOp`](#workspaceop)[] | no | Workspace operations the SIMULATOR performed while rendering this response. Empty when it answered without consulting the workspace. |
| `world_state_after` | object? | no | Present for write tools: world state after this exchange's patch was applied. Sibling exchanges are simulated in list order. |

### `ToolImplementation`

One caller-authored Lua implementation, as evidence: the tool it serves, its source, and a hash identifying the exact revision that executed.

| Field | Type | Required | Description |
|---|---|---|---|
| `source` | string | yes |  |
| `source_hash` | string | yes |  |
| `tool` | string | yes |  |

### `TraceTurn`

One PUT model completion. Text, thinking, and every tool request emitted by that completion stay together, preserving the model's actual turn boundary. An empty `tool_exchanges` list is a text-only/final completion.

| Field | Type | Required | Description |
|---|---|---|---|
| `model_output` | string | yes | The model's text output for this completion, or empty when it emitted only tool calls. |
| `thinking` | string? | no | The PUT model's visible reasoning ("thinking") for this completion, when the provider reports it. Transparency only — it is never fed back into the conversation. |
| `tool_exchanges` | [`ToolExchange`](#toolexchange)[] | yes | All tool calls requested together by this single model completion, paired with their simulated responses. Exchanges retain provider order and are simulated in that order; they are one batch, not separate PUT turns. |

### `TraceView`

| Field | Type | Required | Description |
|---|---|---|---|
| `execution` | [`RunExecution`](#runexecution) | yes | Deterministic termination, consumed budget and monotonic execution timing. A recorded trace need not contain a final agent completion. |
| `final_world_state` | map&lt;string, any&gt; | yes | World state at the end of the trace (after all applied patches). |
| `implementations` | [`ToolImplementation`](#toolimplementation)[] | no | The scenario's caller-supplied Lua implementations, with the source hash each tool_exchanges[].lua_execution refers to. Empty when every response was rendered by the simulator LLM. |
| `resolved_inputs` | map&lt;string, any&gt; | no | The concrete {{variable}} values the simulator generated from the scenario's input_domain and rendered the template with — the exact input that produced this trace, for reproduction. |
| `tool_calls` | integer | yes | Number of tool calls the simulated PUT made in this trace. |
| `turns` | [`TraceTurn`](#traceturn)[] | yes | Structured PUT model turns, rendered as whole turn objects by the UI. Tool calls requested by one completion are nested together. |
| `workflow` | [`WorkflowEvidence`](#workflowevidence)? | no |  |

### `UsageByRole`

Token usage and call counts split by model role: the application side (all workflow agent models, serialized under the historical `put` role name) vs. the tool simulator. These roles serve very different purposes: the simulator is the test ENVIRONMENT and the workflow is the application under test, so a single combined total would hide which side is expensive.

| Field | Type | Required | Description |
|---|---|---|---|
| `put` | [`UsageTotals`](#usagetotals) | yes | Usage of ALL agent invocations in the system under test, including repeated stages and different models. USD cost is summed per actual model and unavailable if any used model lacks pricing. |
| `sim` | [`UsageTotals`](#usagetotals) | yes | Usage of the tool-simulator model (the LLM that roleplays the environment — rendering tool responses and resolving inputs). |

### `UsageTotals`

Cumulative usage across every call routed through a `UsageTracker`.

| Field | Type | Required | Description |
|---|---|---|---|
| `cache_read_tokens` | integer | yes |  |
| `cost_usd` | number? | no | Estimated USD cost of this usage, when the server knows the per-token pricing for the model that produced it (e.g. OpenRouter models). Absent for subscription / no-pricing providers and for models the catalog doesn't price. The tracker never sets this (it sees tokens, not prices); the server fills it in from the model catalog when assembling a response. |
| `input_tokens` | integer | yes |  |
| `llm_calls` | integer | yes | Completions requested across all roles (the runner PUT and the tool simulator). |
| `output_tokens` | integer | yes |  |
| `tool_calls` | integer | yes | Tool calls the model requested. Only the simulated PUT has tools, so this counts tool calls in simulated traces. |

### `WorkflowEvidence`

Complete orchestration evidence, retained on the finished trace and in live progress while the workflow is still running or has failed.

| Field | Type | Required | Description |
|---|---|---|---|
| `error` | string? | no |  |
| `invocations` | [`AgentInvocationRecord`](#agentinvocationrecord)[] | no |  |
| `limits` | [`WorkflowLimits`](#workflowlimits)? | no |  |
| `output` | any | no |  |
| `params` | any | yes |  |
| `source` | string | yes |  |
| `source_hash` | string | yes |  |
| `stop_reason` | [`RunStopReason`](#runstopreason)? | no |  |
| `tool_calls` | [`DirectToolCallRecord`](#directtoolcallrecord)[] | no |  |

### `WorkflowLimits`

HARD APPLICATION-ORCHESTRATION limits, NOT scenario tool-handler limits. Exceeding these stops/fails the workflow and retains partial evidence. It NEVER asks the simulator LLM to execute, repair or continue the program. ctx.run_agent never delegates to the simulator either. A scenario tool's own Lua handler may separately fall back to simulation; that distinct policy is governed by the scenario's LuaOptions, not these limits. Direct calls are recorded in workflow.tool_calls; agent conversations in workflow.invocations plus their ranges into the flat turns array.

| Field | Type | Required | Description |
|---|---|---|---|
| `max_agent_invocations` | integer | no | Max ctx.run_agent invocations. |
| `max_direct_tool_calls` | integer | no | Max direct ctx.call_tool invocations. |
| `max_duration_ms` | integer | no | Cooperative wall-clock deadline for Lua CPU time only. |
| `max_host_bytes` | integer | no | Combined serialized argument/result bytes over ctx.run_agent/ctx.call_tool. |
| `max_host_calls` | integer | no | Combined ctx.run_agent/ctx.call_tool call limit. |
| `max_instructions` | integer | no | Shared VM/conversion instruction budget. |
| `max_memory_bytes` | integer | no | Lua allocator limit. |
| `max_result_bytes` | integer | no | Maximum serialized size of one converted value / final workflow output. |
| `max_source_bytes` | integer | no | UTF-8 source length limit before parsing. |
| `max_value_depth` | integer | no | Maximum JSON/Lua nesting depth. |

### `WorkflowProgram`

The APPLICATION under test, owned by the investigation, not the scenario. To test extract -> review, make TWO ctx.run_agent calls in ONE program; do not stitch independent investigations together. Params has no privileged keys. The default program is merely a convention using prompt/model/controls.  This is NOT scenario tool-handler Lua. Its ctx has render, run_agent, call_tool, input and resolved_inputs; NO ctx.workspace, world state, simulator notes, or tool implementation source. To read a file, invoke the scenario tool: ctx.call_tool("read_file", {path="config.json"}), not ctx.workspace.read. Read GET /docs/workflow for executable examples and the full contract.

| Field | Type | Required | Description |
|---|---|---|---|
| `limits` | [`WorkflowLimits`](#workflowlimits) | no |  |
| `lua_source` | string | no | Lua chunk returning function(params, ctx). Omit to run the default program with ordinary params.prompt, params.model and optional params.controls. Custom programs can loop, branch and transform data. ctx.render(text) validates and fills `{{input_domain}}` values. ctx.run_agent{prompt=...,model=...,input=...,tools={...},controls={...}} returns ONE result table: output is nil without a final completion; failure describes an ordinary agent error; stop_reason, invocation_id, steps_used and tokens_used retain execution facts. Use qualified model identifiers from /api/models. Each call gets a fresh conversation; scenario world/workspace and total budgets are shared across calls. Exact input handoffs are recorded, not re-simulated. ctx.call_tool(name, args) returns that scenario tool's response using the same simulation engine. Workflow errors NEVER delegate orchestration to the simulator. |
| `params` | any | no | Arbitrary caller-owned JSON. The harness assigns it no semantics. |

### `WorkspaceLimits`

Per-tool workspace capability bounds. `None` keeps the documented default.

| Field | Type | Required | Description |
|---|---|---|---|
| `max_grep_matches` | integer? | no |  |
| `max_line_len` | integer? | no |  |
| `max_output_bytes` | integer? | no |  |
| `max_read_lines` | integer? | no |  |

### `WorkspaceOp`

One operation the tool SIMULATOR performed against its simulation workspace while rendering a tool response (e.g. it read a file, or grepped, before answering). Supporting provenance, NOT the PUT observation: a successful lookup does not prove the simulated response copied it faithfully. Inspect ToolExchange.response against this record and the narrative. Pure data.

| Field | Type | Required | Description |
|---|---|---|---|
| `args` | any | yes | The arguments the simulator passed (JSON). |
| `result` | any | yes | The result the workspace returned (JSON). Always a value; errors are in-band (e.g. `{"error": "not found"}`). |
| `tool` | string | yes | Which workspace tool: read, write, list_dir, or grep. |
