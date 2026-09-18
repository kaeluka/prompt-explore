# prompt-explore API

Property-based testing for agent behavior. You AUTHOR one scenario (a test case: a world, an input domain, and a protagonist — see the Scenario schema) and submit it with a prompt under test (PUT) and an optional free-form `reason` justifying the run. A job runs that one scenario: the simulator picks concrete inputs from the input domain, renders the world's tools, and the PUT acts in it. The harness then surfaces COMPLETE EVIDENCE — the world, input domain, resolved inputs, and full trace of model turns — or explicit failure evidence. THE CALLER IS THE JUDGE: there is no in-harness verdict. The `reason` justifies the run — what it aims to accomplish, what changed compared to previous runs, what a reader should know (there is no strict standard) — and is surfaced with the result to guide reading the traces; it is not an oracle. Traces are informative even when nothing is obviously wrong; the deliverable is the conversation trace, and the caller reads it and decides what (if anything) to fix. The API is job-based: POST returns a job id immediately; poll GET /api/investigations/{id} for status, then read GET /api/investigations/{id}/evidence for the complete nonduplicated conversation.   WORKED OPTIMIZATION LOOP (the caller does every judgment):  1. GET /api/models lists catalogs, NOT generation readiness or credit balance. Before a large fanout, run one small investigation with both chosen roles. A quota/balance failure needs provider/operator action, not a prompt edit. Keep the simulator configuration stable when comparing PUT prompts/models, but expect each run to simulate afresh; identical settings do NOT pin responses.  2. POST one scenario per investigation; record campaign/variant attributes. For code tools, specify root paths, literal versus regex search, and response shape in their descriptions/world. Do not assume host workspace grep implements the same semantics as your invented tool. Include difficult negative controls.  3. Poll, then GET /api/investigations/{id}/evidence. Read execution.stop_reason, budget and timing first: done means a trace was recorded, not necessarily a final answer. Read turns in order, including EVERY tool_exchanges[].call AND response. The response is the PUT observation; workspace_ops is only supporting provenance. lua_execution=computed means code executed, NOT that the response is faithful. A final correct answer can hide invalid root listings, false-empty searches or invented files. Inspect generated revisions too; rerunning regenerates code. Do not grade fidelity from outcome counts or final answers alone. Preserve simulation limitations in your assessment; withhold unsupported grades.  4. Record the judgment in the product, not only local prose: PATCH the id with grades and assessment. Example: {"grades":{"quality":0.5},"assessment":{"summary":"Correct conclusion, but incomplete evidence","rubric":"quality: 0..1, higher is better; one inspected case, not a precision estimate","evidence":[{"turn":0,"exchange":0,"note":"The actual tool response contradicts the promised root listing"}]}}. Adapt the score and references to what actually happened; the example is NOT a grading algorithm. Use assessment alone when no numeric grade is justified. Grades are caller-owned; the harness validates only shape and reference bounds. When a new assessment invalidates earlier scores, clear those stale grades in the SAME PATCH (for example grades:{grounded:null,quality:null}) or replace them with justified values. An assessment warning alone does not remove old scores from the frontier; never plot a discredited grade as if it were current evidence.  5. POST /api/frontier with explicit grouping for the variables you compare. Backend example: {"group_by":["campaign","variant","simulation_backend","step_budget","token_budget"],"axes":[{"name":"quality","better":"higher"},{"name":"sim_cost_usd","better":"lower"}]}. All stored jobs remain candidates. A listing/card filter never limits frontier candidacy; unrelated/null groups and excluded members remain explicit. Defaults group by PUT settings/prompt, so they MERGE different simulator backends unless you add the backend key. The caller owns corpus comparability and grade scales.  6. Hand off a shareable dashboard URL using URL-encoded JSON query values group_by (array of attribute names), axes (array of name/better objects), and attributes (exact string matches for CARDS ONLY). Example before URL encoding: /?group_by=["campaign","variant","simulation_backend"]&axes=[{"name":"quality","better":"higher"},{"name":"sim_cost_usd","better":"lower"}]&attributes={"campaign":"trial"}. Never put bearer tokens in the URL. Archive evidence/requests for durability: jobs and caller annotations are in memory and lost on restart.   DESIGN INTENT — why it works this way:  • Scenarios are world SPECIFICATIONS, not instantiated data. A narrative pins what exists (inventory; facts, including NEGATIVE facts; completeness assertions; rendering rules) and the simulator lazily renders concrete tool responses from it. Materializing a full environment requires a closed world (enumerable, bounded, copyable); open worlds — web search, email, a payment network — can never be materialized, so a narrative (prose) is the only mechanism that generalizes. This is why a scenario is a spec, not a fixture.  • Tool responses are SIMULATED from the narrative. By default every response is rendered by the LLM. Experimental opt-in conversation_controls.lua_simulation={} lets that same simulator specialize optional Lua tool implementations before the PUT loop and during later fallbacks. This accelerates computations, NOT a cache or a semantic correctness guarantee. Unimplemented inputs delegate through PleaseSimulateException; runtime errors delegate with explicit error evidence and rolled-back Lua writes. The generated source and revision history are visible in simulation_program, beside resolved_inputs, on both progress and result.trace. Each exchange records lua_execution when tried. All computed/LLM responses enter the same simulator conversation. Progress reports its phase: resolving_inputs, preparing_tools, or put_loop. Lua has only bounded workspace capabilities, no host IO, randomness, or clock. The caller judges code and traces against the narrative; example_responses remain realism hints, NOT pinned outputs.  • The answer to simulation unreliability is TRANSPARENCY, not enforcement. Every tool response is in the trace and the caller sees the same narrative, so a response that contradicts the stated facts is VISIBLE for the caller to read. Divergence is SURFACED, not silently fixed.  • Because tool responses are LLM-simulated, an investigation MAY contain unrealistic or WRONG results — responses that contradict the narrative, invent facts, or drift across calls. The harness does NOT vet them (there is no judge). It is the CALLER'S responsibility to read the traces and double-check the simulated tool responses thoroughly. When simulation quality is insufficient, iterate with two levers and re-run the same scenarios: (a) sharpen the scenario NARRATIVE — tighter facts and negative facts; (b) use a stronger SIM_MODEL — it must be powerful enough to simulate believably.  THE SIMULATION WORKSPACE (optional, closed-world materialization). POST /api/investigations also accepts `multipart/form-data` with an optional `workspace` part: a .zip decompressed ENTIRELY IN MEMORY (never on disk) that seeds an in-memory filesystem the tool SIMULATOR consults. Narratives remain the only mechanism that generalizes (open worlds can't be materialized), but a zip IS a closed world — so when you have one (a repo slice, a corpus of articles, a mailbox export) you can hand it over so the simulator can consult authoritative bytes. This does NOT guarantee its returned reads/greps/listings are faithful; inspect the actual responses even when workspace operations succeeded. The simulator accesses the workspace with four tools — read, write, list_dir, grep — and it is named the "simulation workspace" in its own prompt, so your scenario `world` can address it by that name and instruct it (e.g. "use the write tool to record any generated source code"). The workspace is EPHEMERAL and per-trace (every scenario run gets a fresh copy; the agent under test never sees it — only tool responses). WHEN the simulator uses it is the world narrative's policy, not the harness's: state what the zip contains, where things live, and its completeness stance (closed: "these are ALL the files; anything else is not found"; partial: "these are SOME files; simulate the rest"). Each tool exchange records the simulator's workspace operations (`workspace_ops`) so you can judge whether an answer was grounded in the uploaded files or invented. Caps: ≤ 50 MB compressed, ≤ 500 MB decompressed (overridable via                        PROMPT_EXPLORE_WORKSPACE_{COMPRESSED,DECOMPRESSED}_LIMIT);                        zip-slip entries and files in the reserved .prompt-explore namespace are rejected. That namespace holds private program-authoring artifacts; Lua application workspace capabilities cannot list/read/grep/write it.   AUTHENTICATION. The server is open by default. When PROMPT_EXPLORE_API_TOKEN is set (non-empty), every /api/* route EXCEPT /api/openapi.json requires an `Authorization: Bearer <token>` header (security scheme `api_token`). The web UI prompts for the token and stores it in localStorage.

Version: `0.5.0` — generated from `openapi.json`; do not edit by hand (see `scripts/dump-openapi.sh`).

## Endpoints

### `GET /`

Serve the web UI. Share a view using URL-encoded JSON query parameters: `group_by` is an array of attribute names, `axes` an array of {name,better}, and `attributes` an object of exact string matches for cards ONLY. Filtering cards never changes the all-jobs frontier. The UI copies/restores these view settings without storing a server-side selection. Never put tokens in URLs.

| Status | Response |
|---|---|
| `200` | Web UI (HTML) |

### `POST /api/frontier`

Compute a grouped Pareto frontier over ALL investigations currently held by this server. There is no investigation-selection list: `group_by` chooses the provenance/campaign attributes that define one candidate point (default: put model, thinking setting, and behavior-only prompt hash). Each point retains its member ids and explicit exclusions. Running/failed/ungraded members are successful evidence, not a 422: poll jobs, PATCH grades, then POST this same request again to update preliminary coordinates. A failed run (`result.failure` is present and `result.trace` is null) is a failed exclusion; a done job contributes its single trace. Judging trace adequacy remains the caller's responsibility.

| Parameter | In | Type | Description |
|---|---|---|---|
| `format` | query | string | `json` (default) or `svg` |

Body: [`GroupedFrontierRequest`](#groupedfrontierrequest)

| Field | Type | Required | Description |
|---|---|---|---|
| `axes` | [`FrontierAxis`](#frontieraxis)[] | yes | Axes whose arithmetic means define Pareto dominance. Every included run has every requested value, so different axes never average different cohorts. |
| `group_by` | string[] | no | Attribute names that form a group. Omit for exactly `["put_model", "put_thinking", "prompt_hash"]`; send `[]` for one group containing every current job. A job missing a requested key is retained in that key's explicit JSON-null group, never dropped. |


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

One investigation is one conversation: send `scenario`, not `scenarios`. For different worlds/workspaces or repeated samples, submit independent investigations and group them using attributes. Each repeated submission resolves inputs and simulates afresh; it does not isolate PUT variability with fixed inputs/tool responses. There is no sample-count field, batch endpoint, or reusable workspace handle. Every upload is independent.  Two request shapes are accepted: - `application/json` — the body is an `InvestigateRequest` (no workspace). - `multipart/form-data` — TWO parts: a `request` part whose body is the   `InvestigateRequest` JSON, and an OPTIONAL `workspace` part whose body   is a `.zip` archive. The zip is decompressed ENTIRELY IN MEMORY (never   written to disk) and seeds the SIMULATION WORKSPACE — an in-memory   filesystem the tool SIMULATOR consults with four tools (read, write,   list_dir, grep). Hard caps: the compressed zip must be ≤ 50 MB and   decompress to ≤ 500 MB total (overridable via   PROMPT_EXPLORE_WORKSPACE_{COMPRESSED,DECOMPRESSED}_LIMIT), or the   request is rejected. Zip entries   that escape the workspace root (zip-slip), or use the reserved private   `.prompt-explore` namespace, are rejected.  The workspace is the simulator's CAPABILITY, not a policy. The harness tells the simulator the workspace exists, how many files it contains, and that it is ephemeral (per-trace: every scenario run gets a fresh copy; the agent under test NEVER sees it — only tool responses). WHEN and WHETHER the simulator uses it — including tactics like persisting generated content — is the WORLD NARRATIVE's job: say in the scenario's `world` what the zip contains, where things live, and its completeness stance ("these are ALL the files; anything else is not found" vs "these are SOME files; simulate the rest"). The harness enforces none of that; the simulator's workspace operations appear in each trace tool exchange (`workspace_ops`) so you can judge whether an answer was grounded in the uploaded files or invented.

Body: [`InvestigateRequest`](#investigaterequest)

| Field | Type | Required | Description |
|---|---|---|---|
| `attributes` | map&lt;string, string&gt; | no | Caller-owned campaign attributes. This field is literally `attributes`; there is no `tags` alias and unknown fields are rejected. Keys use `^[a-z][a-z0-9_]{0,63}$`; values are strings up to 1024 UTF-8 bytes. `label` is the special editable display label shown by the UI. POST rejects every system-owned key: `put_model`/`sim_model` are the resolved provider-qualified model names; `put_thinking`/`sim_thinking` are a reasoning keyword or `provider_default`; `prompt_hash` is SHA-256 of canonical PUT template/tools/design_goals (not cosmetic PUT id); and `workspace_hash` is SHA-256 of sorted uploaded workspace path/content pairs (including the stable empty-workspace hash). `simulation_backend` is `llm` or `lua`; `step_budget` and `token_budget` are decimal limits (unbounded token budget is `unlimited`). All are immutable provenance; group by them explicitly when comparing execution configurations. |
| `conversation_controls` | [`ConversationControls`](#conversationcontrols) | no | Per-investigation overrides for LLM conversation controls. Omit a field to use its documented server default. These controls are recorded resolved on the job view so traces remain reproducible. |
| `investigation` | [`Investigation`](#investigation) | yes |  |
| `put` | [`PromptUnderTest`](#promptundertest) | yes |  |
| `put_model` | string? | no | Model for the prompt under test. Omit to use the server default (`glm-5.2`). Provider is selected by namespace prefix, e.g. `zai_coding::glm-5.2`, `open_router::deepseek/...`, `bedrock_sigv4::<model-id>`, `vertex::gemini-2.5-pro`; a bare name uses the server's default provider (`PROMPT_EXPLORE_PROVIDER`). See `GET /api/models` for available namespaced model strings.  This is the model you are TESTING: when experimenting to find which model works well for your prompt, this is the one you vary across runs. Keep `sim_model` fixed while you do (see below), so candidates share simulator configuration, not fixed responses. Each run resolves inputs and renders tools afresh; inspect differences before attributing an outcome solely to the prompt/model. |
| `put_thinking_level` | [`ThinkingLevel`](#thinkinglevel)? | no |  |
| `scenario` | [`Scenario`](#scenario) | yes | The required test case to run: one world specification, input domain, and protagonist. A job represents exactly one scenario; `scenarios` is not a compatibility alias and is rejected as an unknown field. |
| `sim_model` | string? | no | Model for the tool SIMULATOR only (the LLM that roleplays the environment). Omit to use the server default independently of `put_model`; setting `put_model` never changes the simulator.  The simulator is the test ENVIRONMENT, not the thing under test. Two consequences: 1. When tuning which model works well for your prompt, keep    `sim_model` STABLE across runs (vary `put_model`, not this). You    are comparing candidate PUTs. Stable settings reduce confounding, but    every run still simulates afresh and may generate different Lua code.    Inspect actual responses/revisions before attributing differences to PUT. 2. The simulator must be POWERFUL ENOUGH to render a believable    environment — a weak simulator produces inconsistent or    unbelievable tool responses, which corrupts every trace    regardless of how good the PUT is. There is a quality floor    below which results stop being meaningful, even if it's    cheaper. Pick a strong model here and leave it set. |
| `sim_thinking_level` | [`ThinkingLevel`](#thinkinglevel)? | no |  |


| Status | Response |
|---|---|
| `202` | Investigation job created: [`JobCreated`](#jobcreated) |
| `400` | Malformed request body (including legacy `scenarios`), invalid conversation controls, invalid/oversized zip, or a thinking level on a model for which the adapter has no reasoning mapping (e.g. Bedrock Meta). Model-specific unsupported keywords are instead rejected by the provider during execution; poll the job and inspect result.failure. |
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

Poll an investigation job. `progress` is always present (live model turns while running, frozen on completion); `result` is present once the job is `done` (trace, possibly budget-capped) or `failed` (failure evidence). Prefer GET /api/investigations/{id}/evidence for reading/judging: it retains actual tool responses and provenance without duplicating terminal progress. Check execution.stop_reason, not status or nonempty text, for how the run stopped.

| Parameter | In | Type | Description |
|---|---|---|---|
| `id` | path | string | Job id returned by POST /api/investigations |

| Status | Response |
|---|---|
| `200` | Job status + live progress (+ result when done or failed): [`JobView`](#jobview) |
| `401` | Missing or invalid bearer token |
| `404` | Unknown job id |

### `PATCH /api/investigations/{id}`

Both maps have merge semantics: a number/string sets or overwrites and JSON `null` deletes that key. `assessment` is caller-owned summary, rubric and zero-based evidence references: object replaces the whole assessment, null clears it, absent leaves it unchanged. All fields validate before ANY apply. The response echoes FULL updated grades, attributes and assessment. A numeric fidelity grade needs review of ACTUAL tool responses, not merely workspace_ops or computed counts. If simulation is inadequate, record that in assessment and withhold unjustified grades; this is not a harness verdict. Grade names and attribute names use `^[a-z][a-z0-9_]{0,63}$`; grade names cannot be measured axes. The literal measured names are `put_input_tokens`, `put_output_tokens`, `put_cache_read_tokens`, `put_cost_usd`, `sim_input_tokens`, `sim_output_tokens`, `sim_cache_read_tokens`, `sim_cost_usd`, `steps_per_trace_avg`, `steps_per_trace_min`, `steps_per_trace_max`, `steps_per_trace_stdev`, `elapsed_ms`, `resolving_inputs_ms`, `preparing_tools_ms`, and `put_loop_ms`.  PATCH is allowed while a job runs. POST /api/frontier always considers ALL current jobs: running, failed, ungraded, or unavailable members appear as explicit exclusions/backlog in a successful grouped response. A group has null coordinates until it has at least one common complete cohort; poll and PATCH missing grades, then submit the same frontier request again.

| Parameter | In | Type | Description |
|---|---|---|---|
| `id` | path | string | Job id returned by POST /api/investigations |

Body: [`InvestigationPatch`](#investigationpatch)

| Field | Type | Required | Description |
|---|---|---|---|
| `assessment` | [`Assessment`](#assessment)? | no |  |
| `attributes` | object? | no | Caller-owned attribute name → string to set/overwrite, or null to delete. This is `attributes`, never `tags` (unknown fields are rejected). `label` names the job in the UI. It affects group identity only when explicitly selected in `group_by`. System provenance keys are read-only. |
| `grades` | object? | no | Axis name → number to set/overwrite, or null to delete. |


| Status | Response |
|---|---|
| `200` | Updated full grades, attributes and assessment: [`InvestigationPatchView`](#investigationpatchview) |
| `400` | Invalid grades or attributes. Attribute keys use `^[a-z][a-z0-9_]{0,63}$`, values are strings ≤1024 bytes, and immutable provenance keys (`put_model`, `sim_model`, `put_thinking`, `sim_thinking`, `prompt_hash`, `workspace_hash`, `simulation_backend`, `step_budget`, `token_budget`) cannot change. |
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

## Schemas

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

| Field | Type | Required | Description |
|---|---|---|---|
| `max_steps_per_trace` | integer | yes | Max steps per trace. A STEP is one tool call OR one final completion (the turn with no tool call that ends the trace). A completion that requests several tool calls counts as several steps but is an atomic batch: every sibling call is simulated, so one accepted batch may cross this cap. No later PUT turn then runs. The main cost dial for tool-loop PUTs. Reserve room for the final completion. A trace recorded at the cap can lack a final answer; inspect execution.stop_reason and counters rather than equating done with success. |
| `max_tokens` | integer? | no | Optional PUT input+output token cap, summed across completions (repeated conversation history is counted on every completion). Simulator tokens are not part of this cap. See execution.put_tokens_used and stop_reason. |

### `BudgetCutoffCompletion`

A provider completion received after cumulative PUT tokens exceeded the cap. It was charged and is preserved verbatim as evidence, but NOT accepted into the conversation. No tool requests here were executed or given fake responses. Raw argument strings may be malformed; they were not parsed by the runner.

| Field | Type | Required | Description |
|---|---|---|---|
| `model_output` | string? | no |  |
| `thinking` | string? | no |  |
| `tool_calls` | [`ToolCallRequest`](#toolcallrequest)[] | yes |  |

### `ConversationControls`

Caller-selected limits and sampling controls for an investigation's LLM conversations. Defaults: temperature 0.7; PUT/simulator output limits 32768 tokens each; 20 total JSON-reply attempts; 250 workspace turns; 5000 read lines, 1000 grep matches, 2000 characters per grep line, and 1 MiB constructed output per workspace tool call.

| Field | Type | Required | Description |
|---|---|---|---|
| `lua_simulation` | [`LuaOptions`](#luaoptions)? | no |  |
| `max_workspace_turns` | integer? | no | Workspace tool calls per simulator response before a final-answer nudge. |
| `put_max_tokens` | integer? | no | Maximum output tokens per PUT completion; omit for the documented default. |
| `put_temperature` | number? | no | PUT sampling temperature; omit for the documented default. |
| `sim_max_repair_attempts` | integer? | no | Total attempts per simulator JSON reply, including the initial reply (default 20). Empty replies, invalid JSON, and schema mismatches are retried in the same conversation with repair feedback. This is separate from process-level HTTP/transport retries, not a provider retry setting. |
| `sim_max_tokens` | integer? | no | Maximum output tokens per simulator completion; omit for the documented default. |
| `sim_temperature` | number? | no | Simulator sampling temperature; omit for the documented default. |
| `workspace_max_grep_matches` | integer? | no | Matches one simulator workspace `grep` may return. |
| `workspace_max_line_len` | integer? | no | Characters retained from each simulator workspace grep-result line. |
| `workspace_max_output_bytes` | integer? | no | Byte budget used while constructing one simulator workspace tool result. This prevents a huge single-line file or directory from being copied in full before downstream token/Lua limits can reject it. |
| `workspace_max_read_lines` | integer? | no | Lines one simulator workspace `read` may return. |

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
| `name` | string | yes | A graded axis name (you PATCHed it) or a reserved measured axis (harness-computed). Exact reserved names and baked-in directions: `put_input_tokens`, `put_output_tokens`, `sim_input_tokens`, and `sim_output_tokens` (lower); `put_cache_read_tokens` and `sim_cache_read_tokens` (higher — cached input is cheaper); `put_cost_usd` and `sim_cost_usd` (lower); and `steps_per_trace_avg`, `steps_per_trace_min`, `steps_per_trace_max`, `steps_per_trace_stdev` (lower). Monotonic durations `elapsed_ms`, `resolving_inputs_ms`, `preparing_tools_ms`, `put_loop_ms` are also lower. Compare latency only across adequate comparable traces, not faster failures. The `put_/sim_` notation is only prose shorthand, NEVER a valid axis name. Requesting a reserved axis with a contradicting `better` is rejected. |

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
| `group_by` | string[] | no | Attribute names that form a group. Omit for exactly `["put_model", "put_thinking", "prompt_hash"]`; send `[]` for one group containing every current job. A job missing a requested key is retained in that key's explicit JSON-null group, never dropped. |

### `GroupedFrontierResponse`

| Field | Type | Required | Description |
|---|---|---|---|
| `points` | [`GroupedFrontierPoint`](#groupedfrontierpoint)[] | yes |  |

### `InvestigateRequest`

| Field | Type | Required | Description |
|---|---|---|---|
| `attributes` | map&lt;string, string&gt; | no | Caller-owned campaign attributes. This field is literally `attributes`; there is no `tags` alias and unknown fields are rejected. Keys use `^[a-z][a-z0-9_]{0,63}$`; values are strings up to 1024 UTF-8 bytes. `label` is the special editable display label shown by the UI. POST rejects every system-owned key: `put_model`/`sim_model` are the resolved provider-qualified model names; `put_thinking`/`sim_thinking` are a reasoning keyword or `provider_default`; `prompt_hash` is SHA-256 of canonical PUT template/tools/design_goals (not cosmetic PUT id); and `workspace_hash` is SHA-256 of sorted uploaded workspace path/content pairs (including the stable empty-workspace hash). `simulation_backend` is `llm` or `lua`; `step_budget` and `token_budget` are decimal limits (unbounded token budget is `unlimited`). All are immutable provenance; group by them explicitly when comparing execution configurations. |
| `conversation_controls` | [`ConversationControls`](#conversationcontrols) | no | Per-investigation overrides for LLM conversation controls. Omit a field to use its documented server default. These controls are recorded resolved on the job view so traces remain reproducible. |
| `investigation` | [`Investigation`](#investigation) | yes |  |
| `put` | [`PromptUnderTest`](#promptundertest) | yes |  |
| `put_model` | string? | no | Model for the prompt under test. Omit to use the server default (`glm-5.2`). Provider is selected by namespace prefix, e.g. `zai_coding::glm-5.2`, `open_router::deepseek/...`, `bedrock_sigv4::<model-id>`, `vertex::gemini-2.5-pro`; a bare name uses the server's default provider (`PROMPT_EXPLORE_PROVIDER`). See `GET /api/models` for available namespaced model strings.  This is the model you are TESTING: when experimenting to find which model works well for your prompt, this is the one you vary across runs. Keep `sim_model` fixed while you do (see below), so candidates share simulator configuration, not fixed responses. Each run resolves inputs and renders tools afresh; inspect differences before attributing an outcome solely to the prompt/model. |
| `put_thinking_level` | [`ThinkingLevel`](#thinkinglevel)? | no |  |
| `scenario` | [`Scenario`](#scenario) | yes | The required test case to run: one world specification, input domain, and protagonist. A job represents exactly one scenario; `scenarios` is not a compatibility alias and is rejected as an unknown field. |
| `sim_model` | string? | no | Model for the tool SIMULATOR only (the LLM that roleplays the environment). Omit to use the server default independently of `put_model`; setting `put_model` never changes the simulator.  The simulator is the test ENVIRONMENT, not the thing under test. Two consequences: 1. When tuning which model works well for your prompt, keep    `sim_model` STABLE across runs (vary `put_model`, not this). You    are comparing candidate PUTs. Stable settings reduce confounding, but    every run still simulates afresh and may generate different Lua code.    Inspect actual responses/revisions before attributing differences to PUT. 2. The simulator must be POWERFUL ENOUGH to render a believable    environment — a weak simulator produces inconsistent or    unbelievable tool responses, which corrupts every trace    regardless of how good the PUT is. There is a quality floor    below which results stop being meaningful, even if it's    cheaper. Pick a strong model here and leave it set. |
| `sim_thinking_level` | [`ThinkingLevel`](#thinkinglevel)? | no |  |

### `InvestigateResponse`

| Field | Type | Required | Description |
|---|---|---|---|
| `failure` | [`RunFailure`](#runfailure)? | yes |  |
| `trace` | [`TraceView`](#traceview)? | yes |  |
| `usage` | [`UsageByRole`](#usagebyrole) | yes | Cumulative token usage and call counts, split by the prompt under test (`put`) and the simulator (`sim`). Present even when `failure` is set. |

### `Investigation`

An investigation's controls: run one supplied scenario against the PUT and surface its resulting trace. Callers that want a corpus run invoke the singular operation once per scenario. Nothing is judged in-harness — the caller reads the trace and judges.

| Field | Type | Required | Description |
|---|---|---|---|
| `budget` | [`Budget`](#budget) | yes |  |
| `reason` | string? | no | Free-form justification for the run — WHY it exists and what a reader should know when comparing it with earlier runs: what it aims to accomplish, what changed compared to previous runs (a prompt edit, new scenarios, a different model), anything that frames how to read the traces. There is no strict standard — write whatever makes the run intelligible later.  Advisory only: surfaced with the result to guide reading the traces, NEVER used as an oracle. The harness runs a scenario and surfaces evidence; the caller is the judge. Optional — omit it when you just want to observe behavior with no particular framing.  e.g. "baseline before adding the explicit-confirmation rule" or "re-run after softening the refusal instruction; compare with v3". |

### `InvestigationEvidence`

Preferred agent reading surface. One complete conversation without duplicated terminal progress. Read every `turns[].tool_exchanges[].response`: this is what the PUT actually observed. `workspace_ops` only shows what the simulator consulted; `lua_execution.outcome=computed` only says code ran, not that the reply was faithful. Compare replies with `scenario.world` AND `put.tools`. An invalid root listing or false-empty search can invalidate a comparison even when the final answer looks right. Source revisions may differ across reruns.  Available while running and after failure: successful exchanges and setup artifacts are retained. `execution.stop_reason` distinguishes a final completion from a budget cutoff or runtime failure; `status=done` alone does not. Full responses, model/simulator reasoning, workspace operations and Lua source are all preserved here. No semantic compression or inferred correctness flags.

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
| `phase` | [`RunPhase`](#runphase) | yes |  |
| `put` | [`PromptUnderTest`](#promptundertest) | yes |  |
| `put_model` | string | yes |  |
| `put_thinking_level` | [`ThinkingLevel`](#thinkinglevel)? | no |  |
| `reason` | string? | no |  |
| `resolved_inputs` | map&lt;string, any&gt; | yes |  |
| `scenario` | [`Scenario`](#scenario) | yes |  |
| `sim_model` | string | yes |  |
| `sim_thinking_level` | [`ThinkingLevel`](#thinkinglevel)? | no |  |
| `simulation_program` | [`SimulationProgram`](#simulationprogram)? | no |  |
| `started_at` | integer | yes |  |
| `status` | [`JobStatus`](#jobstatus) | yes |  |
| `turns` | [`TraceTurn`](#traceturn)[] | yes | Complete PUT model turns, each with tool arguments AND actual responses. EvidenceReference indices refer directly to this array and its exchanges. |
| `usage` | [`UsageByRole`](#usagebyrole)? | no |  |
| `user_message` | string? | no |  |
| `workspace_files` | integer | yes |  |

### `InvestigationPatch`

PATCH updates independently optional grades, attributes and assessment. Every supplied value validates before anything is applied (atomic update). Maps merge; assessment replaces as a whole, null clears, absent leaves unchanged.

| Field | Type | Required | Description |
|---|---|---|---|
| `assessment` | [`Assessment`](#assessment)? | no |  |
| `attributes` | object? | no | Caller-owned attribute name → string to set/overwrite, or null to delete. This is `attributes`, never `tags` (unknown fields are rejected). `label` names the job in the UI. It affects group identity only when explicitly selected in `group_by`. System provenance keys are read-only. |
| `grades` | object? | no | Axis name → number to set/overwrite, or null to delete. |

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

### `JobView`

| Field | Type | Required | Description |
|---|---|---|---|
| `assessment` | [`Assessment`](#assessment)? | yes |  |
| `attributes` | map&lt;string, string&gt; | yes | Immutable provenance plus caller-owned campaign attributes. `label` is the special editable display label; reserved provenance keys cannot change. |
| `budget` | [`Budget`](#budget) | yes | Original per-conversation budget, retained even for failed/capped runs. |
| `conversation_controls` | [`ResolvedConversationControls`](#resolvedconversationcontrols) | yes | Resolved controls for the PUT and simulator conversations. |
| `finished_at` | integer? | yes | Execution completion epoch milliseconds, null while running. Core monotonic phase timings are in progress.execution; polling/file mtimes are not durations. |
| `grades` | map&lt;string, number&gt; | yes | Caller-graded axes on this investigation (PATCHed via PATCH /api/investigations/{id}). Free-form names, caller-chosen scales (0..1, 1..5, anything); the harness stores them and never interprets them. |
| `id` | string | yes | The job's id (same value as the `{id}` path segment and the id in `JobSummary`). Echoed in the body so a consumer holding only this representation knows which job it is — without it, a dashboard that reconciles a list of views by key has nothing stable to key on and silently falls back to positional matching (which leaks per-item UI state such as an unfolded conversation to whatever job sorts into that slot next). |
| `phase` | [`RunPhase`](#runphase) | yes | Which LLM phase the scenario is currently in (see RunPhase). This is the observable status of the job's LLM work. Mirrors `progress.phase`. |
| `progress` | [`RunProgress`](#runprogress) | yes | Live progress for this scenario, populated while running and frozen when the job finishes. Lets a dashboard show a tool-call log as it happens. |
| `put` | [`PromptUnderTest`](#promptundertest) | yes | The prompt under test. |
| `put_model` | string | yes | The resolved model name that ran the prompt under test (the `put_model` from the request, or the server default). Echoed RESOLVED so a reader knows exactly what produced the traces — including the default, which the request leaves implicit. |
| `put_thinking_level` | [`ThinkingLevel`](#thinkinglevel)? | no |  |
| `reason` | string? | no | The run's free-form `reason` (advisory justification: what the run aims to accomplish, what changed vs. earlier runs, what a reader should know — no strict standard). Optional; surfaced to guide reading the traces. Nothing is judged against it. |
| `result` | [`InvestigateResponse`](#investigateresponse)? | no |  |
| `scenario` | [`Scenario`](#scenario) | yes | The input scenario, by value: its narrative is the ground truth for interpreting the trace and progress. |
| `sim_model` | string | yes | The resolved model name that ran the tool simulator (the `sim_model` from the request, or the server default), resolved independently of `put_model`. The simulator is the test ENVIRONMENT; a reader needs to see it to judge whether it was powerful enough to render the world believably. |
| `sim_thinking_level` | [`ThinkingLevel`](#thinkinglevel)? | no |  |
| `started_at` | integer | yes |  |
| `status` | [`JobStatus`](#jobstatus) | yes |  |
| `workspace_files` | integer | yes | How many files seeded the simulation workspace (0 = no zip upload; the simulator answered from narrative alone). The workspace is an in-memory filesystem the SIMULATOR consults via read/write/list_dir/ grep — it is NOT the PUT's tools. See the endpoint description. |

### `LuaExecutionRecord`

Evidence of a Lua attempt before a tool response. Computed means executed, NOT faithful or correct: code can return an invalid-path error or false-empty search successfully. Inspect ToolExchange.response against the tool contract. A fallback or error is NOT the tool's return value: all staged mutations were discarded and the LLM rendered the actual response. Successful Lua operations appear in the exchange's ordinary workspace_ops instead.

| Field | Type | Required | Description |
|---|---|---|---|
| `detail` | string? | no |  |
| `discarded_workspace_ops` | [`WorkspaceOp`](#workspaceop)[] | no |  |
| `outcome` | [`LuaOutcome`](#luaoutcome) | yes |  |
| `program_revision` | integer | yes |  |

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

### `ModelEntry`

One model the caller can put in a request's `put_model` or `sim_model` field. `name` is the full namespaced, pastable string (e.g. `open_router::deepseek/deepseek-v4-flash-0731`).

| Field | Type | Required | Description |
|---|---|---|---|
| `name` | string | yes |  |
| `pricing` | object? | no | Per-token USD pricing as reported by the provider. Keys follow OpenRouter's conventions: `prompt` (input), `completion` (output), `input_cache_read` (cached input). Present only when the provider exposes pricing — absent for subscription endpoints (z.ai coding plan) and providers that don't report it (Bedrock). If future pricing sources are added, reuse these same keys. |

### `ModelsResponse`

Models available to put in a request's `put_model` or `sim_model` field, by provider.  Returns the server defaults plus a map keyed by provider namespace (`zai_coding`, `open_router`, `bedrock_sigv4`, `vertex`). Each provider value is either `{available: {models: [{name, pricing?}]}}` — where `name` is the full pastable, namespaced string (e.g. `open_router::deepseek/deepseek-v4-flash-0731`) — or `{error: "…"}` explaining why that provider couldn't be listed (no API key in the environment, no AWS credentials, region-gated, …). Listing is best-effort and per-provider: one provider failing never breaks the others. Cached for a short time so repeated listing is cheap. This does NOT call generation endpoints or check credit balance: available means catalog/configuration discovery, not usable inference. Smoke-test one small investigation with both chosen roles before a corpus fanout. A 429 can mean exhausted balance rather than transient rate limiting; inspect the error, fix provider funding/permissions, and do not silently switch the simulator.

| Field | Type | Required | Description |
|---|---|---|---|
| `generation_checked` | boolean | yes | Always false: this endpoint lists catalogs/configuration, never makes a charged generation call or checks credit balance. `available` is NOT a readiness guarantee. Run one small investigation with both chosen roles before fanout; quota/balance failures need provider/operator action. |
| `providers` | map&lt;string, [`ProviderModels`](#providermodels)&gt; | yes |  |
| `server_default_model` | string | yes | Model used when a request omits `put_model` (a bare name; the server resolves it via `server_default_provider`). |
| `server_default_provider` | string | yes | Provider applied to bare model names when no namespace is given (from PROMPT_EXPLORE_PROVIDER). Maps to a namespace prefix: `zai` -> `zai_coding::`, `zai_standard` -> `zai::`, `openrouter` -> `open_router::`, `bedrock` -> `bedrock_sigv4::`, `gemini` -> `vertex::`. |

### `ProgramRevision`

| Field | Type | Required | Description |
|---|---|---|---|
| `error` | string? | no |  |
| `source` | string | yes | Lua source, displayed as data, never executed by the browser. If error reports an oversized/non-UTF8 file this is a bounded preview, not an executable replacement; that revision always falls back to the LLM. |

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
| `available` | object | yes | Catalog/configuration discovery succeeded or can be attempted. This does NOT verify generation, model access or credit balance. A listed model can still fail its first completion (including 429 insufficient balance). `models` may be empty after a listing failure (see note); the catalog is advisory, not a gate. Run one small investigation with both chosen roles before fanout; this endpoint never makes hidden charged generation probes. |

**Variant**

| Field | Type | Required | Description |
|---|---|---|---|
| `error` | object | yes | Discovery failed, for example absent credentials, network or region errors. This is a listing diagnostic, not a generation-readiness test. |

### `ResolvedConversationControls`

Actual controls after request and server defaults have been resolved.

| Field | Type | Required | Description |
|---|---|---|---|
| `lua_simulation` | [`LuaOptions`](#luaoptions)? | no |  |
| `max_workspace_turns` | integer | yes |  |
| `put_max_tokens` | integer? | no |  |
| `put_temperature` | number? | no |  |
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
| `put_tokens_used` | integer | yes |  |
| `steps_used` | integer | yes |  |
| `stop_reason` | [`RunStopReason`](#runstopreason)? | yes |  |
| `timing` | [`RunTiming`](#runtiming) | no |  |

### `RunFailure`

A failure while running one scenario. `stage` identifies the runtime layer (`"runner"` for PUT execution, input resolution, or tool simulation) and `error` is its diagnostic text.

| Field | Type | Required | Description |
|---|---|---|---|
| `error` | string | yes |  |
| `stage` | string | yes |  |

### `RunPhase`

The LLM phase of the single scenario currently being run. Exposed so a reader can see live work rather than a bare "running" status.

Values: `resolving_inputs`, `preparing_tools`, `put_loop`

### `RunProgress`

Live progress for one scenario. The runner updates this flat value as work proceeds; if the run fails, already resolved inputs, program revisions, and completed turns remain available as evidence.

| Field | Type | Required | Description |
|---|---|---|---|
| `execution` | [`RunExecution`](#runexecution) | no | Deterministic execution evidence. `snapshot()` refreshes its live clock; after `finish()` it is frozen for completed and failed runs. |
| `phase` | [`RunPhase`](#runphase) | yes | The current LLM phase for this scenario. |
| `resolved_inputs` | map&lt;string, any&gt; | no | Concrete template values selected from the input domain. Recorded before the PUT loop so a later failure still exposes reproducible inputs. |
| `simulation_program` | [`SimulationProgram`](#simulationprogram)? | no |  |
| `turns` | [`TraceTurn`](#traceturn)[] | no | Completed PUT model turns accumulated so far. If a sibling tool call fails, the final turn may contain only that completion's successfully rendered exchanges; no failed exchange is invented. |
| `user_message` | string? | no | The opening user message, for rendering the complete conversation live. |

### `RunStopReason`

Why a run stopped. This is deterministic execution bookkeeping, not a verdict on the trace.

Values: `final_completion`, `step_budget`, `token_budget`, `runtime_failure`

### `RunTiming`

Monotonic wall-clock time spent in each observable LLM phase.

| Field | Type | Required | Description |
|---|---|---|---|
| `elapsed_ms` | integer | yes |  |
| `preparing_tools_ms` | integer | yes |  |
| `put_loop_ms` | integer | yes |  |
| `resolving_inputs_ms` | integer | yes |  |

### `Scenario`

A test case: a world specification, an input domain, and a protagonist. A pure VALUE — it carries no identity (`id`); runs report it back by value. The harness runs the prompt under test inside this world and surfaces the resulting trace for the caller to judge.  Scenarios are authored OUTSIDE the harness (by the operator's agent); this API never generates them.  ## Your role: adversary  Your job is to BREAK the prompt under test, not validate it. Assume it is flawed, and construct each scenario — world, input domain, opening turn — to make the bad behavior under investigation SURFACE if that flaw exists. Write the world the way a red-teamer would, not the way the prompt's author would: set the trap (an order that belongs to a DIFFERENT customer; an ownership claim that cannot be verified; a broken lookup) rather than a comfortable situation where the agent easily behaves well. A scenario that lets the agent succeed proves nothing.  If you are an LLM (or are using LLMs) to author scenarios, note that they are notoriously bad at questioning their own output: the same context that wrote (or is reading) the prompt tends to construct scenarios that confirm it rather than break it. A SEPARATE agent helps — construct each scenario with a SUBAGENT if you have one: a fresh context, given only the prompt, the run's `reason`, and this adversary role, is not invested in the prompt and will find angles its author didn't think to defend. This is only a PARTIAL mitigation, not a complete counter — a subagent shares the same model weights and can under-appreciate the same weaknesses — but it is a meaningful start. The mechanics below are tools for this role.  ## Authoring the `world`  The world is ground truth for the simulator AND the caller (who reads the traces and judges), and it is the single biggest determinant of result quality. It must pin four things, all in natural language:    1. INVENTORY — what exists and where, covering every query type the      PUT's tools allow.   2. FACTS — including NEGATIVE facts: what does NOT exist, what NEVER      happens. Models default to inventing positive content; absences      must be stated, and they are often what makes a trace decidable.   3. COMPLETENESS ASSERTIONS — "these are ALL the entry points" (closed      world) or "these are the relevant results" (open world).   4. RENDERING RULES — refuse queries outside the inventory; filler      introduces no new facts; never contradict the facts.  ## Authoring the `input_domain`  For each `{{variable}}` in the PUT template, describe its input DOMAIN — the value space, semantics, and any PRECONDITIONS or trust contract the prompt may assume about it. The simulator picks a concrete value from this domain (its job), fills the template, and the chosen value is reported in the trace's `resolved_inputs`. A domain is richer than a pinned value: "tier is standard or premium, premium cancels without a fee" or "user_record: { id, name, tier }; user.id has been verified upstream — the agent may trust the person described". The world states the contract; whether the world actually HONORS it (or breaks it) is where the behavior you are looking for lives.  Variables are for what VARIES per scenario. If a passage is the same in every scenario, it is not a variable: it is part of the prompt under test and belongs verbatim in the template. (Writing a complete literal as the domain description tends to make the simulator copy it — but it may still paraphrase or drop it; that failure mode is invisible unless you diff `resolved_inputs` against what you sent.)

| Field | Type | Required | Description |
|---|---|---|---|
| `input_domain` | map&lt;string, string&gt; | no | Per-`{{variable}}` input-domain descriptions: the value space, semantics, and preconditions/trust contracts. Each KEY must match a `{{variable}}` placeholder in the PUT template (see `PromptUnderTest.template` for the placeholder syntax); the simulator generates a concrete value for each and substitutes it (reported in the trace's `resolved_inputs`). Only use placeholders for inputs that should VARY across scenarios — constant text under test belongs verbatim in the template, where the simulator cannot paraphrase or drop it. Empty for templates with no placeholders. |
| `simulator_notes` | string | no | Persona/stance guidance for a simulated user, if the scenario involves one. Defaults empty. |
| `user_message` | string? | no | The opening message from the user/protagonist. |
| `world` | string | yes | The world specification — ground truth the simulator renders tool responses from and the caller checks claims against. A SPECIFICATION (prose), not instantiated data. See the API description's DESIGN INTENT. Cover inventory, facts (incl. negatives), completeness, and rendering rules.  If the tools expose a REAL system with authoritative documentation (an OpenAPI spec, a man page, a CLI's --help), EMBED that documentation in the world verbatim and pin the rendering rules to it: "the embedded spec is authoritative for every rendered response." Without it the simulator invents plausible-but-wrong behavior for the documented surface (wrong error codes, invented fields, impossible operations) — verified by A/B: simulated API calls invented 409 read-only errors and off-schema bodies until the real spec was embedded, after which responses matched the contract. The same applies to any authoritative doc: embed it, then pin rendering to it. |

### `SideEffect`

Values: `read`, `write`

### `SimulationProgram`

A generated executable simulation, not an oracle. The narrative remains ground truth; the caller judges whether this code implements it faithfully. Code may be specialized during setup or later LLM fallbacks. All revisions are retained so each exchange identifies the exact implementation it tried.

| Field | Type | Required | Description |
|---|---|---|---|
| `path` | string | yes |  |
| `revisions` | [`ProgramRevision`](#programrevision)[] | yes | Zero-based revisions, including the initial fallback-only module. |
| `setup_thinking` | string? | no |  |
| `setup_workspace_ops` | [`WorkspaceOp`](#workspaceop)[] | no |  |

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

### `ToolSchema`

| Field | Type | Required | Description |
|---|---|---|---|
| `description` | string | yes | The PUT's actual tool contract, also used by the simulator. Describe argument semantics AND returned shape: for repository tools, define root aliases, literal vs regex search (and grammar), line numbering, errors, truncation and which files belong to the inventory. A vague 'pattern' lets LLM and Lua implementations disagree silently. Example: 'path . or empty means root; search is literal case-sensitive substring; return {matches:[{path,line,text}],truncated}; no match is an empty array'. These are caller-supplied semantics, not a built-in harness tool surface. |
| `example_responses` | string[] | no | Realism hints for the simulator LLM. These are anchors/examples, NOT pinned outputs — the simulator renders its own concrete responses from the narrative (see the API description's DESIGN INTENT). In experimental hybrid mode it may generate executable Lua handlers; these examples remain hints, not forced return values. |
| `name` | string | yes |  |
| `parameters` | any | yes | JSON Schema for the tool's parameters. |
| `side_effect` | [`SideEffect`](#sideeffect) | yes |  |

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
| `execution` | [`RunExecution`](#runexecution) | yes | Deterministic termination, consumed budget and monotonic execution timing. A recorded trace need not contain a final PUT completion. |
| `final_world_state` | map&lt;string, any&gt; | yes | World state at the end of the trace (after all applied patches). |
| `resolved_inputs` | map&lt;string, any&gt; | no | The concrete {{variable}} values the simulator generated from the scenario's input_domain and rendered the template with — the exact input that produced this trace, for reproduction. |
| `simulation_program` | [`SimulationProgram`](#simulationprogram)? | no |  |
| `tool_calls` | integer | yes | Number of tool calls the simulated PUT made in this trace. |
| `turns` | [`TraceTurn`](#traceturn)[] | yes | Structured PUT model turns, rendered as whole turn objects by the UI. Tool calls requested by one completion are nested together. |

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

One operation the tool SIMULATOR performed against its simulation workspace while rendering a tool response (e.g. it read a file, or grepped, before answering). Supporting provenance, NOT the PUT observation: a successful lookup does not prove the simulated response copied it faithfully. Inspect ToolExchange.response against this record and the narrative. Pure data.

| Field | Type | Required | Description |
|---|---|---|---|
| `args` | any | yes | The arguments the simulator passed (JSON). |
| `result` | any | yes | The result the workspace returned (JSON). Always a value; errors are in-band (e.g. `{"error": "not found"}`). |
| `tool` | string | yes | Which workspace tool: read, write, list_dir, or grep. |
