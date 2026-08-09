# prompt-explore API

Property-based testing for agent behavior. You AUTHOR scenarios (test cases: a world specification plus a protagonist — see the Scenario schema) and submit them with a prompt under test (PUT) and a behavioral question. Every scenario is run: an LLM simulates the world from the scenario's narrative, the PUT acts in it, and a judge evaluates each resulting trace against your question. A witness is a trace where the questioned behavior actually occurred. The deliverable is the witness (or a clean no-witness sweep) plus the traces — the caller finds and owns the fix; this API does not propose fixes. The API is job-based: POST returns a job id immediately; poll GET /api/investigations/{id} for the result.   DESIGN INTENT — why it works this way:  • Scenarios are world SPECIFICATIONS, not instantiated data. A narrative pins what exists (inventory; facts, including NEGATIVE facts; completeness assertions; rendering rules) and the simulator lazily renders concrete tool responses from it. Materializing a full environment requires a closed world (enumerable, bounded, copyable); open worlds — web search, email, a payment network — can never be materialized, so a narrative (prose) is the only mechanism that generalizes. This is why a scenario is a spec, not a fixture.  • Tool responses are SIMULATED by an LLM from the narrative, not scripted. Deterministic / pinned responses (e.g. a `when_called_with` override) are a deliberate NON-GOAL: any fixture or DSL you build fails to express a realistic case, and making the harness own simulation fidelity just swaps LLM flakiness (already accepted) for harness bugs (now your problem). `example_responses` are realism hints for the simulator, NOT pinned outputs.  • The answer to simulation unreliability is TRANSPARENCY, not enforcement. Every tool response is in the trace; the judge sees the same narrative and can flag a response that contradicts the stated facts. Divergence is SURFACED for you to read, not silently fixed.  • Because tool responses are LLM-simulated, an investigation MAY contain unrealistic or WRONG results — responses that contradict the narrative, invent facts, or drift across calls. The harness does NOT vet them. It is the CALLER'S responsibility to read the traces and double-check the simulated tool responses thoroughly before trusting any verdict. When simulation quality is insufficient, iterate with three levers and re-run the same scenarios: (a) sharpen the scenario NARRATIVE — tighter facts and negative facts; (b) use a stronger SIM_MODEL — it must be powerful enough to simulate believably; (c) use a stronger JUDGE_MODEL — so divergence is caught.

Version: `0.1.0` — generated from `openapi.json`; do not edit by hand (see `scripts/dump-openapi.sh`).

## Endpoints

### `GET /`

Serve the web UI.

| Status | Response |
|---|---|
| `200` | Web UI (HTML) |

### `GET /api/investigations`

List all jobs (for the dashboard). Running jobs first, then by recency. Returns summaries only — poll a job's id for full progress.

| Status | Response |
|---|---|
| `200` | All jobs: [`JobSummary`](#jobsummary)[] |

### `POST /api/investigations`

Start an investigation: run every given scenario against the PUT and judge each trace against the question. Runs in the background; poll the returned id. The result includes every attempt (scenario + trace + verdict), any witness, incidental findings, and token usage.

Body: [`InvestigateRequest`](#investigaterequest)

| Field | Type | Required | Description |
|---|---|---|---|
| `investigation` | [`Investigation`](#investigation) | yes |  |
| `judge_model` | string? | no | Model for the JUDGE only (the LLM that evaluates each trace against your question). Defaults to `model`. The judge is the safety-critical role: a weak judge fails to catch what a weak PUT does, so it should be at least as strong as the PUT, ideally stronger. Splitting it out lets you keep a strong judge while you vary the PUT model — and a stronger judge also catches simulator divergence (tool responses that contradict the narrative). |
| `model` | string? | no | Model for every LLM role (runner PUT + judge). Omit to use the server default (`glm-5.2`). Provider is selected by namespace prefix, e.g. `zai_coding::glm-5.2`, `open_router::deepseek/...`, `bedrock_sigv4::<model-id>`; a bare name uses the server's default provider (`PROMPT_EXPLORE_PROVIDER`). See `GET /api/models` for available namespaced model strings.  This is the model you are TESTING: when experimenting to find which model works well for your prompt, this is the one you vary across runs. Keep `sim_model` fixed while you do (see below), so each candidate PUT is judged in the same simulated environment. |
| `put` | [`PromptUnderTest`](#promptundertest) | yes |  |
| `scenarios` | [`Scenario`](#scenario)[] | yes | The test cases to run. Required; ALL of them are run (an explicit list is a contract — the step/token budget applies per trace, not to the count). Scenarios are authored outside this API and are editable before running: reviewing them is the intended workflow. |
| `sim_model` | string? | no | Model for the tool SIMULATOR only (the LLM that roleplays the environment). Defaults to `model`.  The simulator is the test ENVIRONMENT, not the thing under test. Two consequences: 1. When tuning which model works well for your prompt, keep    `sim_model` STABLE across runs (vary `model`, not this). You    are comparing candidate PUTs; the environment must stay fixed    so differences in the traces come from the PUT, not from a    shifting simulation. 2. The simulator must be POWERFUL ENOUGH to render a believable    environment — a weak simulator produces inconsistent or    unbelievable tool responses, which corrupts every trace    regardless of how good the PUT is. There is a quality floor    below which results stop being meaningful, even if it's    cheaper. Pick a strong model here and leave it set. |


| Status | Response |
|---|---|
| `202` | Investigation job created: [`JobCreated`](#jobcreated) |

### `GET /api/investigations/{id}`

Poll an investigation job. `progress` is always present (live steps while running, frozen when done); `result` is present once done.

| Parameter | In | Type | Description |
|---|---|---|---|
| `id` | path | string | Job id returned by POST /api/investigations |

| Status | Response |
|---|---|
| `200` | Job status + live progress (+ result when done): [`JobView`](#jobview) |
| `404` | Unknown job id |

### `GET /api/models`

| Status | Response |
|---|---|
| `200` | Available models per provider: [`ModelsResponse`](#modelsresponse) |

## Schemas

### `AttemptView`

| Field | Type | Required | Description |
|---|---|---|---|
| `final_world_state` | map&lt;string, any&gt; | yes | World state at the end of the trace (after all applied patches). |
| `matched` | boolean | yes |  |
| `narrative` | string | yes | The scenario's narrative (world spec), so the consumer can judge simulation quality alongside the trace. |
| `scenario_id` | string | yes | The scenario this attempt ran, by id — ties the evidence back to its world. |
| `steps` | [`TraceStep`](#tracestep)[] | yes | Structured steps, rendered as HTML by the UI. |
| `tool_calls` | integer | yes | Number of tool calls the simulated PUT made in this trace. |
| `user_message` | string? | no |  |
| `verdict_confidence` | number? | no |  |
| `verdict_rationale` | string | yes |  |

### `Attribution`

| Field | Type | Required | Description |
|---|---|---|---|
| `evidence` | string | yes | Free-text attribution note (e.g. the scenario id that produced the witness). |
| `instruction_spans` | string[] | yes | Verbatim quoted substrings of the PUT template implicated in the behavior. Currently always empty: with caller-provided scenarios there is no hypothesis to attribute instruction spans from. |

### `Budget`

| Field | Type | Required | Description |
|---|---|---|---|
| `max_steps_per_trace` | integer | yes | Max steps per trace. A STEP is one tool call OR one final completion (the turn with no tool call that ends the trace). A completion that requests several tool calls counts as several steps. The main cost dial for tool-loop PUTs. |
| `max_tokens` | integer? | no | Optional per-trace token cap (input+output, summed across turns). |

### `InvestigateRequest`

| Field | Type | Required | Description |
|---|---|---|---|
| `investigation` | [`Investigation`](#investigation) | yes |  |
| `judge_model` | string? | no | Model for the JUDGE only (the LLM that evaluates each trace against your question). Defaults to `model`. The judge is the safety-critical role: a weak judge fails to catch what a weak PUT does, so it should be at least as strong as the PUT, ideally stronger. Splitting it out lets you keep a strong judge while you vary the PUT model — and a stronger judge also catches simulator divergence (tool responses that contradict the narrative). |
| `model` | string? | no | Model for every LLM role (runner PUT + judge). Omit to use the server default (`glm-5.2`). Provider is selected by namespace prefix, e.g. `zai_coding::glm-5.2`, `open_router::deepseek/...`, `bedrock_sigv4::<model-id>`; a bare name uses the server's default provider (`PROMPT_EXPLORE_PROVIDER`). See `GET /api/models` for available namespaced model strings.  This is the model you are TESTING: when experimenting to find which model works well for your prompt, this is the one you vary across runs. Keep `sim_model` fixed while you do (see below), so each candidate PUT is judged in the same simulated environment. |
| `put` | [`PromptUnderTest`](#promptundertest) | yes |  |
| `scenarios` | [`Scenario`](#scenario)[] | yes | The test cases to run. Required; ALL of them are run (an explicit list is a contract — the step/token budget applies per trace, not to the count). Scenarios are authored outside this API and are editable before running: reviewing them is the intended workflow. |
| `sim_model` | string? | no | Model for the tool SIMULATOR only (the LLM that roleplays the environment). Defaults to `model`.  The simulator is the test ENVIRONMENT, not the thing under test. Two consequences: 1. When tuning which model works well for your prompt, keep    `sim_model` STABLE across runs (vary `model`, not this). You    are comparing candidate PUTs; the environment must stay fixed    so differences in the traces come from the PUT, not from a    shifting simulation. 2. The simulator must be POWERFUL ENOUGH to render a believable    environment — a weak simulator produces inconsistent or    unbelievable tool responses, which corrupts every trace    regardless of how good the PUT is. There is a quality floor    below which results stop being meaningful, even if it's    cheaper. Pick a strong model here and leave it set. |

### `InvestigateResponse`

| Field | Type | Required | Description |
|---|---|---|---|
| `attempts` | [`AttemptView`](#attemptview)[] | yes | Every completed run — the evidence behind a negative result. |
| `result` | [`RunResult`](#runresult) | yes |  |
| `scenarios_run` | integer | yes | How many of the input scenarios completed a trace. |
| `usage` | [`UsageTotals`](#usagetotals) | yes | Cumulative token usage and call counts across the whole run. |
| `witness_user_message` | string? | no | The opening user message of the witness scenario, so the UI can show the full conversation (the trace steps start with the agent's first reply). |

### `Investigation`

An investigation: run the given scenarios against the PUT and judge every trace against the question.

| Field | Type | Required | Description |
|---|---|---|---|
| `budget` | [`Budget`](#budget) | yes |  |
| `question` | string | yes | The mandatory behavioral question, used VERBATIM as the judge's criterion — e.g. "are there inputs that cause destructive tool calls?" or "why does this sometimes cancel, sometimes ask to confirm?" A witness is a trace where the judge finds the questioned behavior actually occurred. |

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
| `phase` | [`RunPhase`](#runphase) | yes | Which LLM phase the investigation is currently in (see RunPhase: scenarios / checking_goals / proposing). This is the observable status of the job's LLM work — a job may read `status: running` with every scenario done while in `checking_goals` (the advisory design-goal tail). Mirrors `progress.phase`. |
| `progress` | [`RunProgress`](#runprogress) | yes | Live progress — per-scenario state + steps simulated so far. Populated while running; frozen (all scenarios done/failed) when the job finishes. Lets a dashboard show a tool-call log as it happens. |
| `put` | [`PromptUnderTest`](#promptundertest) | yes | The prompt under test. |
| `question` | string | yes | The investigation question (the judge's criterion). |
| `result` | [`InvestigateResponse`](#investigateresponse)? | no |  |
| `scenarios` | [`Scenario`](#scenario)[] | yes | The full input scenarios (narrative = ground truth, etc.). |
| `started_at` | integer | yes |  |
| `status` | [`JobStatus`](#jobstatus) | yes |  |

### `ModelEntry`

One model the caller can put in a request's `model` field. `name` is the full namespaced, pastable string (e.g. `open_router::deepseek/deepseek-v4-flash-0731`).

| Field | Type | Required | Description |
|---|---|---|---|
| `name` | string | yes |  |
| `pricing` | object? | no | Per-token USD pricing as reported by the provider. Keys follow OpenRouter's conventions: `prompt` (input), `completion` (output), `input_cache_read` (cached input). Present only when the provider exposes pricing — absent for subscription endpoints (z.ai coding plan) and providers that don't report it (Bedrock). If future pricing sources are added, reuse these same keys. |

### `ModelsResponse`

Models available to put in a request's `model` field, by provider.  Returns the server defaults plus a map keyed by provider namespace (`zai_coding`, `open_router`, `bedrock_sigv4`). Each provider value is either `{available: {models: [{name, pricing?}]}}` — where `name` is the full pastable, namespaced string (e.g. `open_router::deepseek/deepseek-v4-flash-0731`) — or `{error: "…"}` explaining why that provider couldn't be listed (no API key in the environment, no AWS credentials, region-gated, …). Listing is best-effort and per-provider: one provider failing never breaks the others. Cached for a short time so repeated listing is cheap.

| Field | Type | Required | Description |
|---|---|---|---|
| `providers` | map&lt;string, [`ProviderModels`](#providermodels)&gt; | yes |  |
| `server_default_model` | string | yes | Model used when a request omits `model` (a bare name; the server resolves it via `server_default_provider`). |
| `server_default_provider` | string | yes | Provider applied to bare model names when no namespace is given (from PROMPT_EXPLORE_PROVIDER). Maps to a namespace prefix: `zai` -> `zai_coding::`, `zai_standard` -> `zai::`, `openrouter` -> `open_router::`, `bedrock` -> `bedrock_sigv4::`. |

### `PromptUnderTest`

One prompt under test: the system-prompt template, input variables, tool surface, and (mandatory) design goals. The harness executes this prompt inside scenario worlds and judges the resulting traces.

| Field | Type | Required | Description |
|---|---|---|---|
| `design_goals` | string | yes | MANDATORY. The author's stated intent for the prompt — the yardstick it's supposed to uphold, and itself an optimization target. Advisory in the current verdict: the judge's criterion is the `question` alone; design goals are not automatically enforced during a run. |
| `id` | string | yes |  |
| `input_vars` | map&lt;string, [`VarSpec`](#varspec)&gt; | no | Documents the template's expected `{{variables}}` and how to generate values. With scenarios authored externally, this is metadata for authors; concrete values come from each scenario's `resolved_inputs`, which the runner substitutes into the template. Optional (defaults empty) — it documents intent but does not drive the run. |
| `template` | string | yes | The system-prompt template. `{{var}}` placeholders are substituted from the scenario's `resolved_inputs`. The opening user turn is separate — it comes from the scenario's `user_message`, not the template. |
| `tools` | [`ToolSchema`](#toolschema)[] | yes | This prompt's tool surface, exactly as the model sees it. Empty = no tool loop (but intent lives in `design_goals`, not here). |

### `ProviderModels`

A provider's listing result.

**Variant**

| Field | Type | Required | Description |
|---|---|---|---|
| `available` | object | yes | The provider was queried successfully. |

**Variant**

| Field | Type | Required | Description |
|---|---|---|---|
| `error` | object | yes | The provider could not be queried — e.g. no API key in the environment, no AWS credentials, network error, region-gated. |

### `RunPhase`

The LLM phase an investigation is currently in. Exposed so a reader can see what the job is doing while it runs — never just a bare "running". See the API description: every LLM phase is an observable status.

Values: `scenarios`, `checking_goals`

### `RunProgress`

Live progress of a run, exposed while it's in flight: one entry per scenario, with its steps accumulated as they're simulated. The runner pushes; the server/UI poll and render (e.g. a tool-call log, collapsed by default).

| Field | Type | Required | Description |
|---|---|---|---|
| `phase` | [`RunPhase`](#runphase) | yes | Which LLM phase the investigation is currently in. While the job is running this is the current phase; when it ends it stays at the last phase (the terminal signal is the job status). |
| `scenarios` | [`ScenarioProgress`](#scenarioprogress)[] | yes |  |

### `RunResult`

| Field | Type | Required | Description |
|---|---|---|---|
| `failures` | [`ScenarioFailure`](#scenariofailure)[] | no | Scenarios that errored instead of producing a judged trace (PUT execution, tool simulation, or judge failure). When non-empty, `attempts` may be shorter than `scenarios_tried`; when ALL scenarios failed, `status` is `error`. |
| `final_state` | object? | no | The world state at the end: the witness trace's when one was found, otherwise the last completed attempt's. Informational. |
| `incidental_findings` | string[] | yes | Advisory design-goal violations found across completed traces — best-effort: skipped when `design_goals` is empty or the goal judge errors. These do NOT affect the witness verdict (the question is the sole criterion); they are surfaced for the operator to read. |
| `scenarios_tried` | integer | yes | Scenarios attempted (= completed, in the response's `attempts`, + failed, in `failures`). |
| `status` | [`RunStatus`](#runstatus) | yes |  |
| `strategies_tried` | string[] | yes | Per-scenario provenance labels (e.g. "caller-provided scenario 'id'"), surfaced so negative results show what was tried. |
| `witness` | [`Witness`](#witness)? | no |  |

### `RunStatus`

Values: `witness_found`, `no_witness_within_budget`, `partial`, `error`

### `Scenario`

A test case: a world specification plus a protagonist. The harness runs the prompt under test inside this world — the simulator LLM renders tool responses from the `narrative` — and judges whether the questioned behavior occurred in the resulting trace.  Scenarios are authored OUTSIDE the harness (by the operator's agent); this API never generates them.  ## Authoring the `narrative`  The narrative is ground truth for the simulator AND the judge, and it is the single biggest determinant of result quality. It must pin four things, all in natural language:    1. INVENTORY — what exists and where, covering every query type the      PUT's tools allow (files/paths for a repo agent; orders and their      states for a support agent; per-topic results for a search tool).   2. FACTS — including NEGATIVE facts: what does NOT exist, what NEVER      happens. Models default to inventing positive content; absences      must be stated, and they are often what makes the witness      decidable.   3. COMPLETENESS ASSERTIONS — "these are ALL the entry points" (a      closed world) or "these are the relevant results on this topic"      (an open world). Without one, the simulator may invent extra      content that looks like a real finding.   4. RENDERING RULES — refuse queries outside the inventory; filler      introduces no new facts; never contradict the facts.  Size the world to the step budget: a small world fully explored beats a large world half-explored. And vary the worlds across a scenario set — same-shape scenarios prove the same thing twice.  Good vs bad: a narrative that only says "a customer service bot with an order database" lets the simulator invent whatever order exists (so a witness is meaningless). A good narrative names the order id, states it belongs to a DIFFERENT customer (negative fact), asserts that is the ONLY order (completeness), and says to refuse unknown ids (rendering rule) — then a cancellation that ignores ownership is a genuine witness. See the POST /api/investigations request example.  Everything needed to (stochastically) reproduce a trajectory lives here; everything else in a trace is derived.

| Field | Type | Required | Description |
|---|---|---|---|
| `id` | string | yes | Free-form label, echoed back in reports. |
| `narrative` | string | yes | The world specification — ground truth the simulator renders tool responses from, and the judge checks claims against. A narrative is a SPECIFICATION (prose), not instantiated data: open worlds (web, email, payments) can never be materialized, so the simulator lazily renders concrete responses from it. See the API description's DESIGN INTENT for why this is prose and not a fixture. Required; every scenario must pin one — cover (1) inventory, (2) facts including NEGATIVE facts, (3) completeness assertions, (4) rendering rules. |
| `resolved_inputs` | map&lt;string, any&gt; | no | Concrete values for the PUT template's {{variables}}. Empty for templates with no placeholders. |
| `simulator_notes` | string | no | Persona/stance guidance for a simulated user, if the scenario involves one. Defaults empty. |
| `stated_state` | string? | no | Operator-required environment facts for THIS scenario (e.g. "cancel_order is broken and returns E_CONN"). Appended to the simulator's context so it respects them. Independent per scenario; there is no investigation-level environment-state field. |
| `user_message` | string? | no | The opening message from the user/protagonist. For a tool-less PUT this is the entire work input. |
| `world_state` | map&lt;string, any&gt; | no | Mutable world facts, updated by write-tool patches during the trace. Static truth belongs in the narrative, not here. Defaults empty. |

### `ScenarioFailure`

A scenario that errored during a run.

| Field | Type | Required | Description |
|---|---|---|---|
| `error` | string | yes | The error message. |
| `scenario_id` | string | yes |  |
| `stage` | string | yes | Where it failed: `"runner"` (PUT execution or tool simulation) or `"judge"`. |

### `ScenarioProgress`

One scenario's progress within a run.

| Field | Type | Required | Description |
|---|---|---|---|
| `scenario_id` | string | yes |  |
| `state` | [`ScenarioState`](#scenariostate) | yes |  |
| `steps` | [`TraceStep`](#tracestep)[] | yes | Steps simulated so far (tool calls + responses + model output). |
| `user_message` | string? | no | The opening user message (the protagonist's first turn, played by the simulator side). Lets a chat view render the whole conversation, not just from the PUT's first reply. |

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
| `matched` | boolean | yes |  |

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

### `Trace`

| Field | Type | Required | Description |
|---|---|---|---|
| `final_world_state` | map&lt;string, any&gt; | no | The world state at the end of the run (after all applied patches). Empty if no write tool ever ran. |
| `scenario_id` | string | yes |  |
| `steps` | [`TraceStep`](#tracestep)[] | yes |  |
| `verdict` | [`Verdict`](#verdict)? | no |  |

### `TraceStep`

| Field | Type | Required | Description |
|---|---|---|---|
| `model_output` | string | yes | The model's text output for this turn (empty on non-first tool calls within one completion). |
| `tool_call` | [`ToolCall`](#toolcall)? | no |  |
| `tool_response` | any | no | The simulated tool response. |
| `world_state_after` | object? | no | Present on write-tool steps: world state after the patch applied. |

### `UsageTotals`

Cumulative usage across every call routed through a `UsageTracker`.

| Field | Type | Required | Description |
|---|---|---|---|
| `cache_read_tokens` | integer | yes |  |
| `input_tokens` | integer | yes |  |
| `llm_calls` | integer | yes | Completions requested across all roles (hypothesizer, builder, runner PUT + simulator, judge). |
| `output_tokens` | integer | yes |  |
| `tool_calls` | integer | yes | Tool calls the model requested. Only the simulated PUT has tools, so this counts tool calls in simulated traces. |

### `VarSpec`

Extensible per-variable data-generation spec.  New variants must be additive; the serialized form stays self-describing via the `kind` tag.

**Variant `constant`**

| Field | Type | Required | Description |
|---|---|---|---|
| `kind` | `constant` | yes |  |
| `value` | any | yes |  |

**Variant `nl_description`**

| Field | Type | Required | Description |
|---|---|---|---|
| `description` | string | yes |  |
| `kind` | `nl_description` | yes |  |

**Variant `examples`**

| Field | Type | Required | Description |
|---|---|---|---|
| `examples` | any[] | yes |  |
| `kind` | `examples` | yes |  |

### `Verdict`

| Field | Type | Required | Description |
|---|---|---|---|
| `confidence` | number? | no | The judge's confidence in its own verdict (self-reported). |
| `matched` | boolean | yes | Whether the judge finds the questioned behavior (the investigation's `question`, used verbatim) ACTUALLY occurred in this trace. Design goals are NOT anded in — they're an advisory yardstick and a separate optimization target, not enforced here. |
| `matched_step_indices` | integer[] | yes | Where in the trace the match happened. |
| `rationale` | string | yes |  |

### `Witness`

| Field | Type | Required | Description |
|---|---|---|---|
| `attribution` | [`Attribution`](#attribution) | yes |  |
| `traces` | [`Trace`](#trace)[] | yes | The matching trace. Currently always length 1 (existential mode only); differential/divergence questioning is not implemented. |
