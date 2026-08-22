# prompt-explore API

Property-based testing for agent behavior. You AUTHOR scenarios (test cases: a world, an input domain, and a protagonist — see the Scenario schema) and submit them with a prompt under test (PUT) and an optional free-form `reason` justifying the run. Every scenario is run: the simulator picks concrete inputs from the input domain, renders the world's tools, and the PUT acts in it. The harness then surfaces COMPLETE EVIDENCE for every scenario — the world, the input domain, the resolved inputs, and the full trace of steps. THE CALLER IS THE JUDGE: there is no in-harness verdict. The `reason` justifies the run — what it aims to accomplish, what changed compared to previous runs, what a reader should know (there is no strict standard) — and is surfaced with the result to guide reading the traces; it is not an oracle. Traces are informative even when nothing is obviously wrong; the deliverable is the set of traces, and the caller reads them and decides what (if anything) to fix. The API is job-based: POST returns a job id immediately; poll GET /api/investigations/{id} for the result.   DESIGN INTENT — why it works this way:  • Scenarios are world SPECIFICATIONS, not instantiated data. A narrative pins what exists (inventory; facts, including NEGATIVE facts; completeness assertions; rendering rules) and the simulator lazily renders concrete tool responses from it. Materializing a full environment requires a closed world (enumerable, bounded, copyable); open worlds — web search, email, a payment network — can never be materialized, so a narrative (prose) is the only mechanism that generalizes. This is why a scenario is a spec, not a fixture.  • Tool responses are SIMULATED by an LLM from the narrative, not scripted. Deterministic / pinned responses (e.g. a `when_called_with` override) are a deliberate NON-GOAL: any fixture or DSL you build fails to express a realistic case, and making the harness own simulation fidelity just swaps LLM flakiness (already accepted) for harness bugs (now your problem). `example_responses` are realism hints for the simulator, NOT pinned outputs.  • The answer to simulation unreliability is TRANSPARENCY, not enforcement. Every tool response is in the trace and the caller sees the same narrative, so a response that contradicts the stated facts is VISIBLE for the caller to read. Divergence is SURFACED, not silently fixed.  • Because tool responses are LLM-simulated, an investigation MAY contain unrealistic or WRONG results — responses that contradict the narrative, invent facts, or drift across calls. The harness does NOT vet them (there is no judge). It is the CALLER'S responsibility to read the traces and double-check the simulated tool responses thoroughly. When simulation quality is insufficient, iterate with two levers and re-run the same scenarios: (a) sharpen the scenario NARRATIVE — tighter facts and negative facts; (b) use a stronger SIM_MODEL — it must be powerful enough to simulate believably.  THE SIMULATION WORKSPACE (optional, closed-world materialization). POST /api/investigations also accepts `multipart/form-data` with an optional `workspace` part: a .zip decompressed ENTIRELY IN MEMORY (never on disk) that seeds an in-memory filesystem the tool SIMULATOR consults. Narratives remain the only mechanism that generalizes (open worlds can't be materialized), but a zip IS a closed world — so when you have one (a repo slice, a corpus of articles, a mailbox export) you can hand it over and the simulator answers reads/greps/listings truthfully instead of inventing them. The simulator accesses the workspace with four tools — read, write, list_dir, grep — and it is named the "simulation workspace" in its own prompt, so your scenario `world` can address it by that name and instruct it (e.g. "use the write tool to record any generated source code"). The workspace is EPHEMERAL and per-trace (every scenario run gets a fresh copy; the agent under test never sees it — only tool responses). WHEN the simulator uses it is the world narrative's policy, not the harness's: state what the zip contains, where things live, and its completeness stance (closed: "these are ALL the files; anything else is not found"; partial: "these are SOME files; simulate the rest"). Each trace step records the simulator's workspace operations (`workspace_ops`) so you can judge whether an answer was grounded in the uploaded files or invented. Caps: ≤ 5 MB compressed, ≤ 50 MB decompressed; zip-slip entries are rejected.   AUTHENTICATION. The server is open by default. When PROMPT_EXPLORE_API_TOKEN is set (non-empty), every /api/* route EXCEPT /api/openapi.json requires an `Authorization: Bearer <token>` header (security scheme `api_token`). The web UI prompts for the token and stores it in localStorage.

Version: `0.3.1` — generated from `openapi.json`; do not edit by hand (see `scripts/dump-openapi.sh`).

## Endpoints

### `GET /`

Serve the web UI.

| Status | Response |
|---|---|
| `200` | Web UI (HTML) |

### `POST /api/frontier`

Dominance is N-dimensional; `format=svg` renders exactly 2 axes (a v0 rendering constraint — send `format=json` for N axes). Axis direction is declared HERE, per request (`"better": "lower" \| "higher"`), never stored. The harness records your judgment and does arithmetic; it never interprets a grade — including which graded values are "good enough" or what an axis should measure. Those are caller-domain questions, answered when you PATCH grades from the traces.  Every fixable problem (missing grade, unpriced cost axis, running job, unknown id, duplicate id, bad label/color, direction conflict with a reserved axis, …) comes back in ONE 422 body with typed reasons, each detail naming the fix — including the exact PATCH to make for a missing grade.

| Parameter | In | Type | Description |
|---|---|---|---|
| `format` | query | string | `json` (default) or `svg` |

Body: [`FrontierRequest`](#frontierrequest)

| Field | Type | Required | Description |
|---|---|---|---|
| `axes` | [`FrontierAxis`](#frontieraxis)[] | yes | The axes to compute dominance over. `format=svg` requires EXACTLY 2 (a v0 rendering constraint — the dominance math is N-dimensional); `format=json` accepts any count ≥ 1. |
| `investigations` | [`FrontierInvestigation`](#frontierinvestigation)[] | yes | The investigations to plot (each must be a `done` job with a value for every axis). Bare id strings or `{id, label?, color?}` objects. Ids must be UNIQUE — duplicates are rejected. Labels: `^[[A-Za-z0-9_-]{1,64}$`; colors: `#rrggbb`. Defaults: label = the PUT's id (deduplicated) else the uuid prefix; color = a deterministic palette by position. |


| Status | Response |
|---|---|
| `200` | Frontier points. `format=json` (default): body = FrontierResponse (points with values, on_frontier, dominated_by — uuids, not labels, are the stable key). `format=svg`: body is `image/svg+xml`, a scatter plot with the non-dominated staircase; lower-is-better axes are pixel-inverted so up-and-right is always better.: [`FrontierResponse`](#frontierresponse) |
| `400` | Malformed body or unknown ?format |
| `401` | Missing or invalid bearer token |
| `422` | Fixable problems, all collected: every detail names the fix (for a missing grade, the exact PATCH to make). Reasons: unknown_investigation, duplicate_investigation, job_running, job_failed, no_grade, axis_absent, direction_conflict, bad_axis_name, duplicate_axis, axis_arity, bad_label, bad_color, empty_investigations, empty_axes: [`FrontierError`](#frontiererror) |

### `GET /api/investigations`

List all jobs (for the dashboard). Running jobs first, then by recency. Returns summaries only — poll a job's id for full progress.

| Status | Response |
|---|---|
| `200` | All jobs: [`JobSummary`](#jobsummary)[] |
| `401` | Missing or invalid bearer token |

### `POST /api/investigations`

Two request shapes are accepted: - `application/json` — the body is an `InvestigateRequest` (no workspace). - `multipart/form-data` — TWO parts: a `request` part whose body is the   `InvestigateRequest` JSON, and an OPTIONAL `workspace` part whose body   is a `.zip` archive. The zip is decompressed ENTIRELY IN MEMORY (never   written to disk) and seeds the SIMULATION WORKSPACE — an in-memory   filesystem the tool SIMULATOR consults with four tools (read, write,   list_dir, grep). Hard caps: the compressed zip must be ≤ 5 MB and   decompress to ≤ 50 MB total, or the request is rejected. Zip entries   that escape the workspace root (zip-slip) are rejected.  The workspace is the simulator's CAPABILITY, not a policy. The harness tells the simulator the workspace exists, how many files it contains, and that it is ephemeral (per-trace: every scenario run gets a fresh copy; the agent under test NEVER sees it — only tool responses). WHEN and WHETHER the simulator uses it — including tactics like persisting generated content — is the WORLD NARRATIVE's job: say in the scenario's `world` what the zip contains, where things live, and its completeness stance ("these are ALL the files; anything else is not found" vs "these are SOME files; simulate the rest"). The harness enforces none of that; the simulator's workspace operations appear in each trace step (`workspace_ops`) so you can judge whether an answer was grounded in the uploaded files or invented.

Body: [`InvestigateRequest`](#investigaterequest)

| Field | Type | Required | Description |
|---|---|---|---|
| `investigation` | [`Investigation`](#investigation) | yes |  |
| `model` | string? | no | Model for every LLM role (the PUT runner and the tool simulator). Omit to use the server default (`glm-5.2`). Provider is selected by namespace prefix, e.g. `zai_coding::glm-5.2`, `open_router::deepseek/...`, `bedrock_sigv4::<model-id>`, `vertex::gemini-2.5-pro`; a bare name uses the server's default provider (`PROMPT_EXPLORE_PROVIDER`). See `GET /api/models` for available namespaced model strings.  This is the model you are TESTING: when experimenting to find which model works well for your prompt, this is the one you vary across runs. Keep `sim_model` fixed while you do (see below), so each candidate PUT runs in the same simulated environment. |
| `put` | [`PromptUnderTest`](#promptundertest) | yes |  |
| `scenarios` | [`Scenario`](#scenario)[] | yes | The test cases to run. Required; ALL of them are run (an explicit list is a contract — the step/token budget applies per trace, not to the count). Scenarios are authored outside this API and are editable before running: reviewing them is the intended workflow. |
| `sim_model` | string? | no | Model for the tool SIMULATOR only (the LLM that roleplays the environment). Defaults to `model`.  The simulator is the test ENVIRONMENT, not the thing under test. Two consequences: 1. When tuning which model works well for your prompt, keep    `sim_model` STABLE across runs (vary `model`, not this). You    are comparing candidate PUTs; the environment must stay fixed    so differences in the traces come from the PUT, not from a    shifting simulation. 2. The simulator must be POWERFUL ENOUGH to render a believable    environment — a weak simulator produces inconsistent or    unbelievable tool responses, which corrupts every trace    regardless of how good the PUT is. There is a quality floor    below which results stop being meaningful, even if it's    cheaper. Pick a strong model here and leave it set. |


| Status | Response |
|---|---|
| `202` | Investigation job created: [`JobCreated`](#jobcreated) |
| `400` | Malformed request body or invalid/oversized zip |
| `401` | Missing or invalid bearer token |

### `DELETE /api/investigations/{id}`

Delete an investigation: remove the job — its traces, grades, and progress — from the server's memory. Irreversible: the evidence is gone (a re-run means POSTing a new investigation), and grades are only stored on the job — read the job first if you want to keep them. Useful for pruning a campaign's dead variants so the dashboard and POST /api/frontier only show the points you still compare. RUNNING jobs cannot be deleted (409): a run cannot be cancelled — its provider calls would keep spending while the result is discarded. Poll until done or failed, then delete.

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

Poll an investigation job. `progress` is always present (live steps while running, frozen when done); `result` is present once done.

| Parameter | In | Type | Description |
|---|---|---|---|
| `id` | path | string | Job id returned by POST /api/investigations |

| Status | Response |
|---|---|
| `200` | Job status + live progress (+ result when done): [`JobView`](#jobview) |
| `401` | Missing or invalid bearer token |
| `404` | Unknown job id |

### `PATCH /api/investigations/{id}`

Grade by READING the traces with your full goal in mind. The reason grading is the caller's job (not the harness's, not a script's) is that you hold goal-context that does not compress into words: mechanical stand-ins (regexes over summaries, extractors) approximate judgment and drift badly. Use scripts to FIND the moments worth judging — never to decide. Prefer axes that VARY across your variants: an axis every investigation scores the same on cannot separate anything on a frontier; saturating axes usually mean the scenarios are too easy, not that the variants tie.  Merge semantics per axis: a number sets/overwrites, `null` deletes. The response echoes the FULL updated grades map. Axis names must match `^[a-z][a-z0-9_]{0,63}$` and must not collide with a reserved measured axis (put_/sim_input_tokens, put_/sim_output_tokens, put_/sim_cache_read_tokens, put_/sim_cost_usd, steps_per_trace_ {avg,min,max,stdev}) — those are harness-computed and cannot be graded. Any scale is fine (0..1, 1..5, raw counts): dominance only needs comparability across points, and direction is declared per request at frontier time, not here.  Grading is allowed in any job state (live-tagging while the run unfolds is fine) — but POST /api/frontier only accepts `done` jobs as points.

| Parameter | In | Type | Description |
|---|---|---|---|
| `id` | path | string | Job id returned by POST /api/investigations |

Body: [`GradesPatch`](#gradespatch)

| Field | Type | Required | Description |
|---|---|---|---|
| `grades` | map&lt;string, number?&gt; | yes | Axis name → value. Use JSON `null` to DELETE an axis. Axis names must match `^[a-z][a-z0-9_]{0,63}$` and must not collide with a reserved measured axis (see the frontier docs). |


| Status | Response |
|---|---|
| `200` | Updated grades (full map echoed): [`GradesView`](#gradesview) |
| `400` | Invalid grades (bad axis name, reserved axis name, non-finite value) — every problem is collected into one body that names the fix: [`GradesPatchError`](#gradespatcherror) |
| `401` | Missing or invalid bearer token |
| `404` | Unknown job id |

### `GET /api/models`

| Status | Response |
|---|---|
| `200` | Available models per provider: [`ModelsResponse`](#modelsresponse) |
| `401` | Missing or invalid bearer token |

## Schemas

### `AttemptView`

| Field | Type | Required | Description |
|---|---|---|---|
| `final_world_state` | map&lt;string, any&gt; | yes | World state at the end of the trace (after all applied patches). |
| `resolved_inputs` | map&lt;string, any&gt; | no | The concrete {{variable}} values the simulator generated from the scenario's input_domain and rendered the template with — the exact input that produced this trace, for reproduction. |
| `scenario` | [`Scenario`](#scenario) | yes | The scenario this attempt ran, BY VALUE (no id) — the attempt is self-describing: here is the world, the input domain, the opening turn, and the trace they produced. |
| `steps` | [`TraceStep`](#tracestep)[] | yes | Structured steps, rendered as HTML by the UI. |
| `tool_calls` | integer | yes | Number of tool calls the simulated PUT made in this trace. |

### `BetterDirection`

Whether lower or higher values are better on an axis. Supplied by the caller per request for graded axes; baked in for reserved ones. Dominance normalizes internally (negating lower-is-better values), so "higher score = better" uniformly.

Values: `lower`, `higher`

### `Budget`

| Field | Type | Required | Description |
|---|---|---|---|
| `max_steps_per_trace` | integer | yes | Max steps per trace. A STEP is one tool call OR one final completion (the turn with no tool call that ends the trace). A completion that requests several tool calls counts as several steps. The main cost dial for tool-loop PUTs. |
| `max_tokens` | integer? | no | Optional per-trace token cap (input+output, summed across turns). |

### `FrontierAxis`

One axis of the frontier plot, with the caller's direction.

| Field | Type | Required | Description |
|---|---|---|---|
| `better` | [`BetterDirection`](#betterdirection) | yes | Whether lower or higher values are better on this axis. For graded axes this is YOUR call (encode direction in your own scale, e.g. grade "repeatability" high-good rather than "variance" low-good); for reserved axes it must match the measured direction. |
| `name` | string | yes | A graded axis name (you PATCHed it) or a reserved measured axis (harness-computed). Reserved names and their baked-in directions: put_/sim_input_tokens (lower), put_/sim_output_tokens (lower), put_/sim_cache_read_tokens (higher — cached input is cheaper input), put_/sim_cost_usd (lower), sim_cost_usd (lower), steps_per_trace_avg/_min/_max/_stdev (lower). Requesting a reserved axis with a contradicting `better` is rejected. |

### `FrontierError`

| Field | Type | Required | Description |
|---|---|---|---|
| `error` | string | yes |  |
| `problems` | [`FrontierProblem`](#frontierproblem)[] | yes |  |

### `FrontierInvestigation`

An investigation referenced by the frontier request: a bare id, or an object carrying an optional plot label and color.

**Variant**

`string`

**Variant**

| Field | Type | Required | Description |
|---|---|---|---|
| `color` | string? | no |  |
| `id` | string | yes |  |
| `label` | string? | no |  |

### `FrontierPoint`

One point of the frontier result.

| Field | Type | Required | Description |
|---|---|---|---|
| `color` | string | yes |  |
| `dominated_by` | string[] | yes | Investigations that dominate this point (empty when on the frontier). Tells an optimizer exactly what to compare against. |
| `investigation` | string | yes | The investigation uuid (uuids, not labels, are the stable key — labels are not unique by design). |
| `label` | string | yes |  |
| `on_frontier` | boolean | yes | True when no other point dominates this one. Ties dominate nothing: equal points are both on the frontier. |
| `values` | map&lt;string, number&gt; | yes | Resolved value per axis name. |

### `FrontierProblem`

One fixable problem in a frontier request. Every `detail` names the fix — including, for missing grades, the exact PATCH to make.

| Field | Type | Required | Description |
|---|---|---|---|
| `axis` | string? | no |  |
| `detail` | string | yes |  |
| `investigation` | string? | no |  |
| `reason` | string | yes |  |

### `FrontierRequest`

| Field | Type | Required | Description |
|---|---|---|---|
| `axes` | [`FrontierAxis`](#frontieraxis)[] | yes | The axes to compute dominance over. `format=svg` requires EXACTLY 2 (a v0 rendering constraint — the dominance math is N-dimensional); `format=json` accepts any count ≥ 1. |
| `investigations` | [`FrontierInvestigation`](#frontierinvestigation)[] | yes | The investigations to plot (each must be a `done` job with a value for every axis). Bare id strings or `{id, label?, color?}` objects. Ids must be UNIQUE — duplicates are rejected. Labels: `^[[A-Za-z0-9_-]{1,64}$`; colors: `#rrggbb`. Defaults: label = the PUT's id (deduplicated) else the uuid prefix; color = a deterministic palette by position. |

### `FrontierResponse`

| Field | Type | Required | Description |
|---|---|---|---|
| `points` | [`FrontierPoint`](#frontierpoint)[] | yes |  |

### `GradeProblem`

One fixable problem in a grades PATCH. The `detail` names the fix.

| Field | Type | Required | Description |
|---|---|---|---|
| `axis` | string | yes |  |
| `detail` | string | yes |  |
| `reason` | string | yes | `bad_axis_name` \| `reserved_axis_name` \| `non_finite_value` |

### `GradesPatch`

Caller judgment recorded on an investigation: axis name → number. Merge semantics per axis: a number sets/overwrites, `null` deletes.

| Field | Type | Required | Description |
|---|---|---|---|
| `grades` | map&lt;string, number?&gt; | yes | Axis name → value. Use JSON `null` to DELETE an axis. Axis names must match `^[a-z][a-z0-9_]{0,63}$` and must not collide with a reserved measured axis (see the frontier docs). |

### `GradesPatchError`

| Field | Type | Required | Description |
|---|---|---|---|
| `error` | string | yes |  |
| `problems` | [`GradeProblem`](#gradeproblem)[] | yes |  |

### `GradesView`

The echo response: the full, updated grades map.

| Field | Type | Required | Description |
|---|---|---|---|
| `grades` | map&lt;string, number&gt; | yes |  |

### `InvestigateRequest`

| Field | Type | Required | Description |
|---|---|---|---|
| `investigation` | [`Investigation`](#investigation) | yes |  |
| `model` | string? | no | Model for every LLM role (the PUT runner and the tool simulator). Omit to use the server default (`glm-5.2`). Provider is selected by namespace prefix, e.g. `zai_coding::glm-5.2`, `open_router::deepseek/...`, `bedrock_sigv4::<model-id>`, `vertex::gemini-2.5-pro`; a bare name uses the server's default provider (`PROMPT_EXPLORE_PROVIDER`). See `GET /api/models` for available namespaced model strings.  This is the model you are TESTING: when experimenting to find which model works well for your prompt, this is the one you vary across runs. Keep `sim_model` fixed while you do (see below), so each candidate PUT runs in the same simulated environment. |
| `put` | [`PromptUnderTest`](#promptundertest) | yes |  |
| `scenarios` | [`Scenario`](#scenario)[] | yes | The test cases to run. Required; ALL of them are run (an explicit list is a contract — the step/token budget applies per trace, not to the count). Scenarios are authored outside this API and are editable before running: reviewing them is the intended workflow. |
| `sim_model` | string? | no | Model for the tool SIMULATOR only (the LLM that roleplays the environment). Defaults to `model`.  The simulator is the test ENVIRONMENT, not the thing under test. Two consequences: 1. When tuning which model works well for your prompt, keep    `sim_model` STABLE across runs (vary `model`, not this). You    are comparing candidate PUTs; the environment must stay fixed    so differences in the traces come from the PUT, not from a    shifting simulation. 2. The simulator must be POWERFUL ENOUGH to render a believable    environment — a weak simulator produces inconsistent or    unbelievable tool responses, which corrupts every trace    regardless of how good the PUT is. There is a quality floor    below which results stop being meaningful, even if it's    cheaper. Pick a strong model here and leave it set. |

### `InvestigateResponse`

| Field | Type | Required | Description |
|---|---|---|---|
| `attempts` | [`AttemptView`](#attemptview)[] | yes | Every completed run — the evidence. The caller reads these traces and judges; the harness produces no verdict. |
| `result` | [`RunResult`](#runresult) | yes |  |
| `scenarios_run` | integer | yes | How many of the input scenarios completed a trace. |
| `usage` | [`UsageByRole`](#usagebyrole) | yes | Cumulative token usage and call counts across the whole run, split by model role: the prompt under test (`put`) and the tool simulator (`sim`). Read them separately — the sim is the test environment (often the bigger spender, since every tool response and input resolution goes through it), the PUT is the agent under test. |

### `Investigation`

An investigation: run the given scenarios against the PUT and surface the resulting traces. Nothing is judged in-harness — the caller reads the traces and judges.

| Field | Type | Required | Description |
|---|---|---|---|
| `budget` | [`Budget`](#budget) | yes |  |
| `reason` | string? | no | Free-form justification for the run — WHY it exists and what a reader should know when comparing it with earlier runs: what it aims to accomplish, what changed compared to previous runs (a prompt edit, new scenarios, a different model), anything that frames how to read the traces. There is no strict standard — write whatever makes the run intelligible later.  Advisory only: surfaced with the result to guide reading the traces, NEVER used as an oracle. The harness runs scenarios and surfaces evidence; the caller is the judge. Optional — omit it when you just want to observe behavior with no particular framing.  e.g. "baseline before adding the explicit-confirmation rule" or "re-run after softening the refusal instruction; compare with v3". |

### `JobCreated`

| Field | Type | Required | Description |
|---|---|---|---|
| `id` | string | yes |  |

### `JobStatus`

Values: `running`, `done`, `failed`

### `JobSummary`

| Field | Type | Required | Description |
|---|---|---|---|
| `id` | string | yes |  |
| `scenarios` | integer | yes | How many scenarios this job is running. |
| `started_at` | integer | yes |  |
| `status` | [`JobStatus`](#jobstatus) | yes |  |

### `JobView`

| Field | Type | Required | Description |
|---|---|---|---|
| `error` | string? | no |  |
| `grades` | map&lt;string, number&gt; | yes | Caller-graded axes on this investigation (PATCHed via PATCH /api/investigations/{id}). Free-form names, caller-chosen scales (0..1, 1..5, anything); consumed by POST /api/frontier as judged axes alongside the reserved measured ones. The harness stores them and never interprets them. |
| `id` | string | yes | The job's id (same value as the `{id}` path segment and the id in `JobSummary`). Echoed in the body so a consumer holding only this representation knows which job it is — without it, a dashboard that reconciles a list of views by key has nothing stable to key on and silently falls back to positional matching (which leaks per-item UI state such as an unfolded conversation to whatever job sorts into that slot next). |
| `model` | string | yes | The resolved model name that ran the prompt under test (the `model` from the request, or the server default). Echoed RESOLVED so a reader knows exactly what produced the traces — including the default, which the request leaves implicit. |
| `phase` | [`RunPhase`](#runphase) | yes | Which LLM phase the investigation is currently in (see RunPhase: scenarios). This is the observable status of the job's LLM work. Mirrors `progress.phase`. |
| `progress` | [`RunProgress`](#runprogress) | yes | Live progress — per-scenario state + steps simulated so far. Populated while running; frozen (all scenarios done/failed) when the job finishes. Lets a dashboard show a tool-call log as it happens. |
| `put` | [`PromptUnderTest`](#promptundertest) | yes | The prompt under test. |
| `reason` | string? | no | The run's free-form `reason` (advisory justification: what the run aims to accomplish, what changed vs. earlier runs, what a reader should know — no strict standard). Optional; surfaced to guide reading the traces. Nothing is judged against it. |
| `result` | [`InvestigateResponse`](#investigateresponse)? | no |  |
| `scenarios` | [`Scenario`](#scenario)[] | yes | The full input scenarios (narrative = ground truth, etc.). |
| `sim_model` | string | yes | The resolved model name that ran the tool simulator (the `sim_model` from the request, defaulting to the PUT model, then the server default). The simulator is the test ENVIRONMENT; a reader needs to see it to judge whether it was powerful enough to render the world believably. |
| `started_at` | integer | yes |  |
| `status` | [`JobStatus`](#jobstatus) | yes |  |
| `workspace_files` | integer | yes | How many files seeded the simulation workspace (0 = no zip upload; the simulator answered from narrative alone). The workspace is an in-memory filesystem the SIMULATOR consults via read/write/list_dir/ grep — it is NOT the PUT's tools. See the endpoint description. |

### `ModelEntry`

One model the caller can put in a request's `model` field. `name` is the full namespaced, pastable string (e.g. `open_router::deepseek/deepseek-v4-flash-0731`).

| Field | Type | Required | Description |
|---|---|---|---|
| `name` | string | yes |  |
| `pricing` | object? | no | Per-token USD pricing as reported by the provider. Keys follow OpenRouter's conventions: `prompt` (input), `completion` (output), `input_cache_read` (cached input). Present only when the provider exposes pricing — absent for subscription endpoints (z.ai coding plan) and providers that don't report it (Bedrock). If future pricing sources are added, reuse these same keys. |

### `ModelsResponse`

Models available to put in a request's `model` field, by provider.  Returns the server defaults plus a map keyed by provider namespace (`zai_coding`, `open_router`, `bedrock_sigv4`, `vertex`). Each provider value is either `{available: {models: [{name, pricing?}]}}` — where `name` is the full pastable, namespaced string (e.g. `open_router::deepseek/deepseek-v4-flash-0731`) — or `{error: "…"}` explaining why that provider couldn't be listed (no API key in the environment, no AWS credentials, region-gated, …). Listing is best-effort and per-provider: one provider failing never breaks the others. Cached for a short time so repeated listing is cheap.

| Field | Type | Required | Description |
|---|---|---|---|
| `providers` | map&lt;string, [`ProviderModels`](#providermodels)&gt; | yes |  |
| `server_default_model` | string | yes | Model used when a request omits `model` (a bare name; the server resolves it via `server_default_provider`). |
| `server_default_provider` | string | yes | Provider applied to bare model names when no namespace is given (from PROMPT_EXPLORE_PROVIDER). Maps to a namespace prefix: `zai` -> `zai_coding::`, `zai_standard` -> `zai::`, `openrouter` -> `open_router::`, `bedrock` -> `bedrock_sigv4::`, `gemini` -> `vertex::`. |

### `PromptUnderTest`

One prompt under test: the system-prompt template, input variables, tool surface, and design goals. The harness executes this prompt inside scenario worlds and surfaces the resulting traces for the caller to judge.

| Field | Type | Required | Description |
|---|---|---|---|
| `design_goals` | string | yes | The author's stated intent for the prompt — documentation the caller reads when judging traces. No longer judged in-harness (the judge was removed): it is surfaced with the result as framing, not enforced. Still an optimization target for the caller, who holds the intent. |
| `id` | string | yes |  |
| `template` | string | yes | The system-prompt template. Placeholders use double braces: `{{variable_name}}`. Rules: - Name charset: `[A-Za-z0-9_]` (alphanumeric + underscore). - No spaces inside the braces — write `{{tier}}`, not `{{ tier }}`. - Each placeholder MUST have a matching key in the scenario's   `input_domain`; the simulator generates a concrete value for it   and substitutes it (strings inserted raw; other JSON values in   serialized form). - A template with no placeholders needs no `input_domain`.  Variables are placeholders for things meant to VARY per scenario — the simulator LLM invents each concrete value from the domain description. Text under test does NOT belong in a placeholder: bake it into the template verbatim. Routing constant text through a placeholder hands it to the simulator to (re)generate — it may be paraphrased, or silently dropped from `resolved_inputs`, so the episode runs without the very text being tested. When the complete literal already is the intended value, the simulator tends to copy it — but that is a tendency, not a contract. Placeholders are for inputs the scenario should sample, not for the prompt itself.  The opening user turn is separate — it comes from the scenario's `user_message`, not the template. |
| `tools` | [`ToolSchema`](#toolschema)[] | yes | This prompt's tool surface, exactly as the model sees it. Empty = no tool loop (but intent lives in `design_goals`, not here). |

### `ProviderModels`

A provider's listing result.

**Variant**

| Field | Type | Required | Description |
|---|---|---|---|
| `available` | object | yes | The provider is usable. `models` is the live catalog when the provider exposes one; it may be EMPTY when the catalog listing failed but the provider itself works (see `note`) — the model list is advisory, not a gate: any `<namespace>::<model-id>` the API accepts can be used in a request even if absent here. |

**Variant**

| Field | Type | Required | Description |
|---|---|---|---|
| `error` | object | yes | The provider could not be used at all — e.g. no API key in the environment, credentials that don't resolve, network error, region-gated. |

### `RunPhase`

The LLM phase an investigation is currently in. Exposed so a reader can see what the job is doing while it runs — never just a bare "running". See the API description: every LLM phase is an observable status.

Values: `scenarios`

### `RunProgress`

Live progress of a run, exposed while it's in flight: one entry per scenario (positional — index = position in the submitted list), with its steps accumulated as they are simulated. The runner pushes; the server/UI poll and render.

| Field | Type | Required | Description |
|---|---|---|---|
| `phase` | [`RunPhase`](#runphase) | yes | Which LLM phase the investigation is currently in. |
| `scenarios` | [`ScenarioProgress`](#scenarioprogress)[] | yes |  |

### `RunResult`

The outcome of running a set of scenarios against a PUT. The harness's job ends here: every scenario that completed has a trace in `attempts`; every scenario that errored is in `failures`. There is no verdict — the caller reads the traces and judges.

| Field | Type | Required | Description |
|---|---|---|---|
| `failures` | [`ScenarioFailure`](#scenariofailure)[] | no | Scenarios that errored instead of producing a trace (PUT execution, input resolution, or tool simulation). When non-empty, `attempts` may be shorter than `scenarios_tried`; when ALL scenarios failed, `status` is `error`. |
| `final_state` | object? | no | The world state at the end: the last completed attempt's final state. Informational. |
| `scenarios_tried` | integer | yes | Scenarios attempted (= completed, in the response's `attempts`, + failed, in `failures`). |
| `status` | [`RunStatus`](#runstatus) | yes |  |

### `RunStatus`

Run-completion taxonomy. With the judge removed, "completion" is purely about whether scenarios produced traces — nothing is judged against the run's `reason`. The caller judges the traces.

Values: `completed`, `partial`, `error`

### `Scenario`

A test case: a world specification, an input domain, and a protagonist. A pure VALUE — it carries no identity (`id`); runs report it back by value. The harness runs the prompt under test inside this world and surfaces the resulting trace for the caller to judge.  Scenarios are authored OUTSIDE the harness (by the operator's agent); this API never generates them.  ## Your role: adversary  Your job is to BREAK the prompt under test, not validate it. Assume it is flawed, and construct each scenario — world, input domain, opening turn — to make the bad behavior under investigation SURFACE if that flaw exists. Write the world the way a red-teamer would, not the way the prompt's author would: set the trap (an order that belongs to a DIFFERENT customer; an ownership claim that cannot be verified; a broken lookup) rather than a comfortable situation where the agent easily behaves well. A scenario that lets the agent succeed proves nothing.  If you are an LLM (or are using LLMs) to author scenarios, note that they are notoriously bad at questioning their own output: the same context that wrote (or is reading) the prompt tends to construct scenarios that confirm it rather than break it. A SEPARATE agent helps — construct each scenario with a SUBAGENT if you have one: a fresh context, given only the prompt, the run's `reason`, and this adversary role, is not invested in the prompt and will find angles its author didn't think to defend. This is only a PARTIAL mitigation, not a complete counter — a subagent shares the same model weights and can under-appreciate the same weaknesses — but it is a meaningful start. The mechanics below are tools for this role.  ## Authoring the `world`  The world is ground truth for the simulator AND the caller (who reads the traces and judges), and it is the single biggest determinant of result quality. It must pin four things, all in natural language:    1. INVENTORY — what exists and where, covering every query type the      PUT's tools allow.   2. FACTS — including NEGATIVE facts: what does NOT exist, what NEVER      happens. Models default to inventing positive content; absences      must be stated, and they are often what makes a trace decidable.   3. COMPLETENESS ASSERTIONS — "these are ALL the entry points" (closed      world) or "these are the relevant results" (open world).   4. RENDERING RULES — refuse queries outside the inventory; filler      introduces no new facts; never contradict the facts.  ## Authoring the `input_domain`  For each `{{variable}}` in the PUT template, describe its input DOMAIN — the value space, semantics, and any PRECONDITIONS or trust contract the prompt may assume about it. The simulator picks a concrete value from this domain (its job), fills the template, and the chosen value is reported in the trace's `resolved_inputs`. A domain is richer than a pinned value: "tier is standard or premium, premium cancels without a fee" or "user_record: { id, name, tier }; user.id has been verified upstream — the agent may trust the person described". The world states the contract; whether the world actually HONORS it (or breaks it) is where the behavior you are looking for lives.  Variables are for what VARIES per scenario. If a passage is the same in every scenario, it is not a variable: it is part of the prompt under test and belongs verbatim in the template. (Writing a complete literal as the domain description tends to make the simulator copy it — but it may still paraphrase or drop it; that failure mode is invisible unless you diff `resolved_inputs` against what you sent.)

| Field | Type | Required | Description |
|---|---|---|---|
| `input_domain` | map&lt;string, string&gt; | no | Per-`{{variable}}` input-domain descriptions: the value space, semantics, and preconditions/trust contracts. Each KEY must match a `{{variable}}` placeholder in the PUT template (see `PromptUnderTest.template` for the placeholder syntax); the simulator generates a concrete value for each and substitutes it (reported in the trace's `resolved_inputs`). Only use placeholders for inputs that should VARY across scenarios — constant text under test belongs verbatim in the template, where the simulator cannot paraphrase or drop it. Empty for templates with no placeholders. |
| `simulator_notes` | string | no | Persona/stance guidance for a simulated user, if the scenario involves one. Defaults empty. |
| `user_message` | string? | no | The opening message from the user/protagonist. |
| `world` | string | yes | The world specification — ground truth the simulator renders tool responses from and the caller checks claims against. A SPECIFICATION (prose), not instantiated data. See the API description's DESIGN INTENT. Cover inventory, facts (incl. negatives), completeness, and rendering rules.  If the tools expose a REAL system with authoritative documentation (an OpenAPI spec, a man page, a CLI's --help), EMBED that documentation in the world verbatim and pin the rendering rules to it: "the embedded spec is authoritative for every rendered response." Without it the simulator invents plausible-but-wrong behavior for the documented surface (wrong error codes, invented fields, impossible operations) — verified by A/B: simulated API calls invented 409 read-only errors and off-schema bodies until the real spec was embedded, after which responses matched the contract. The same applies to any authoritative doc: embed it, then pin rendering to it. |

### `ScenarioFailure`

A scenario that errored during a run.

| Field | Type | Required | Description |
|---|---|---|---|
| `error` | string | yes | The error message. |
| `scenario` | [`Scenario`](#scenario) | yes |  |
| `stage` | string | yes | Where it failed: `"runner"` (PUT execution, input resolution, or tool simulation). |

### `ScenarioProgress`

One scenario's progress within a run. Positional: index in the parent `scenarios` vec = the scenario's position in the submitted list.

| Field | Type | Required | Description |
|---|---|---|---|
| `resolved_inputs` | map&lt;string, any&gt; | no | The concrete `{{variable}}` values the simulator generated from the scenario's `input_domain` and rendered the PUT template with. Populated as soon as the scenario starts running (before step 1), so it's visible live — the exact input this trace runs with. |
| `state` | [`ScenarioState`](#scenariostate) | yes |  |
| `steps` | [`TraceStep`](#tracestep)[] | yes | Steps simulated so far (tool calls + responses + model output). |
| `user_message` | string? | no | The opening user message (the protagonist's first turn). Lets a chat view render the whole conversation. |

### `ScenarioState`

The state of one scenario within a run.

**Variant `running`**

| Field | Type | Required | Description |
|---|---|---|---|
| `kind` | `running` | yes |  |

**Variant `done`**

| Field | Type | Required | Description |
|---|---|---|---|
| `kind` | `done` | yes |  |

**Variant `failed`**

| Field | Type | Required | Description |
|---|---|---|---|
| `error` | string | yes |  |
| `kind` | `failed` | yes |  |
| `stage` | string | yes |  |

### `SideEffect`

Values: `read`, `write`

### `ToolCall`

| Field | Type | Required | Description |
|---|---|---|---|
| `args` | any | yes |  |
| `name` | string | yes |  |

### `ToolSchema`

| Field | Type | Required | Description |
|---|---|---|---|
| `description` | string | yes |  |
| `example_responses` | string[] | no | Realism hints for the simulator LLM. These are anchors/examples, NOT pinned outputs — the simulator renders its own concrete responses from the narrative (see the API description's DESIGN INTENT: scripted/pinned tool responses are a deliberate non-goal). |
| `name` | string | yes |  |
| `parameters` | any | yes | JSON Schema for the tool's parameters. |
| `side_effect` | [`SideEffect`](#sideeffect) | yes |  |

### `TraceStep`

| Field | Type | Required | Description |
|---|---|---|---|
| `model_output` | string | yes | The model's text output for this turn (empty on non-first tool calls within one completion). |
| `sim_thinking` | string? | no | The SIMULATOR model's visible reasoning while rendering this step's tool response (its whole inner drive: lookups and final answer). Transparency only; `None` when the simulator model reports no reasoning. |
| `thinking` | string? | no | The PUT model's visible reasoning ("thinking") for this completion, when the provider reports it. Transparency only — it is never fed back into the conversation. Present on the first step produced by a completion (same rule as `model_output`). |
| `tool_call` | [`ToolCall`](#toolcall)? | no |  |
| `tool_response` | any | no | The simulated tool response. |
| `workspace_ops` | [`WorkspaceOp`](#workspaceop)[] | no | Workspace operations the SIMULATOR performed while rendering this step's tool response — e.g. it read or grepped the simulation workspace before answering. Lets the caller see whether the response was grounded in the uploaded files or invented. Empty when the simulator answered without consulting the workspace. |
| `world_state_after` | object? | no | Present on write-tool steps: world state after the patch applied. |

### `UsageByRole`

Token usage and call counts split by model role: the prompt under test vs. the tool simulator. The two models serve very different purposes (the sim is the test ENVIRONMENT, the PUT is the thing under test), so their spend is never lumped together — a single combined total would hide which side is expensive.

| Field | Type | Required | Description |
|---|---|---|---|
| `put` | [`UsageTotals`](#usagetotals) | yes | Usage of the prompt-under-test model (the agent being tested). |
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

### `WorkspaceOp`

One operation the tool SIMULATOR performed against its simulation workspace while rendering a tool response (e.g. it read a file, or grepped, before answering). Recorded for the trace so the caller can judge whether an answer was GROUNDED in the workspace (looked up) or INVENTED by the model — transparency, not enforcement. Pure data.

| Field | Type | Required | Description |
|---|---|---|---|
| `args` | any | yes | The arguments the simulator passed (JSON). |
| `result` | any | yes | The result the workspace returned (JSON). Always a value; errors are in-band (e.g. `{"error": "not found"}`). |
| `tool` | string | yes | Which workspace tool: read, write, list_dir, or grep. |
