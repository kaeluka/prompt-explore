# prompt-explore API

Property-based testing for agent behavior. You AUTHOR scenarios (test cases: a world, an input domain, and a protagonist — see the Scenario schema) and submit them with a prompt under test (PUT) and an optional behavioral question. Every scenario is run: the simulator picks concrete inputs from the input domain, renders the world's tools, and the PUT acts in it. The harness then surfaces COMPLETE EVIDENCE for every scenario — the world, the input domain, the resolved inputs, and the full trace of steps. THE CALLER IS THE JUDGE: there is no in-harness verdict. The question is advisory framing — it states what the caller is worried about and is surfaced with the result to guide reading the traces — not an oracle. Traces are informative even when nothing is obviously wrong; the deliverable is the set of traces, and the caller reads them and decides what (if anything) to fix. The API is job-based: POST returns a job id immediately; poll GET /api/investigations/{id} for the result.   DESIGN INTENT — why it works this way:  • Scenarios are world SPECIFICATIONS, not instantiated data. A narrative pins what exists (inventory; facts, including NEGATIVE facts; completeness assertions; rendering rules) and the simulator lazily renders concrete tool responses from it. Materializing a full environment requires a closed world (enumerable, bounded, copyable); open worlds — web search, email, a payment network — can never be materialized, so a narrative (prose) is the only mechanism that generalizes. This is why a scenario is a spec, not a fixture.  • Tool responses are SIMULATED by an LLM from the narrative, not scripted. Deterministic / pinned responses (e.g. a `when_called_with` override) are a deliberate NON-GOAL: any fixture or DSL you build fails to express a realistic case, and making the harness own simulation fidelity just swaps LLM flakiness (already accepted) for harness bugs (now your problem). `example_responses` are realism hints for the simulator, NOT pinned outputs.  • The answer to simulation unreliability is TRANSPARENCY, not enforcement. Every tool response is in the trace and the caller sees the same narrative, so a response that contradicts the stated facts is VISIBLE for the caller to read. Divergence is SURFACED, not silently fixed.  • Because tool responses are LLM-simulated, an investigation MAY contain unrealistic or WRONG results — responses that contradict the narrative, invent facts, or drift across calls. The harness does NOT vet them (there is no judge). It is the CALLER'S responsibility to read the traces and double-check the simulated tool responses thoroughly. When simulation quality is insufficient, iterate with two levers and re-run the same scenarios: (a) sharpen the scenario NARRATIVE — tighter facts and negative facts; (b) use a stronger SIM_MODEL — it must be powerful enough to simulate believably.

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

Start an investigation: run every given scenario against the PUT and surface the resulting traces. There is no judge — the caller reads the traces and judges. Runs in the background; poll the returned id. The result includes every attempt (scenario + trace) and token usage.

Body: [`InvestigateRequest`](#investigaterequest)

| Field | Type | Required | Description |
|---|---|---|---|
| `investigation` | [`Investigation`](#investigation) | yes |  |
| `model` | string? | no | Model for every LLM role (the PUT runner and the tool simulator). Omit to use the server default (`glm-5.2`). Provider is selected by namespace prefix, e.g. `zai_coding::glm-5.2`, `open_router::deepseek/...`, `bedrock_sigv4::<model-id>`; a bare name uses the server's default provider (`PROMPT_EXPLORE_PROVIDER`). See `GET /api/models` for available namespaced model strings.  This is the model you are TESTING: when experimenting to find which model works well for your prompt, this is the one you vary across runs. Keep `sim_model` fixed while you do (see below), so each candidate PUT runs in the same simulated environment. |
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
| `resolved_inputs` | map&lt;string, any&gt; | no | The concrete {{variable}} values the simulator generated from the scenario's input_domain and rendered the template with — the exact input that produced this trace, for reproduction. |
| `scenario` | [`Scenario`](#scenario) | yes | The scenario this attempt ran, BY VALUE (no id) — the attempt is self-describing: here is the world, the input domain, the opening turn, and the trace they produced. |
| `steps` | [`TraceStep`](#tracestep)[] | yes | Structured steps, rendered as HTML by the UI. |
| `tool_calls` | integer | yes | Number of tool calls the simulated PUT made in this trace. |

### `Budget`

| Field | Type | Required | Description |
|---|---|---|---|
| `max_steps_per_trace` | integer | yes | Max steps per trace. A STEP is one tool call OR one final completion (the turn with no tool call that ends the trace). A completion that requests several tool calls counts as several steps. The main cost dial for tool-loop PUTs. |
| `max_tokens` | integer? | no | Optional per-trace token cap (input+output, summed across turns). |

### `InvestigateRequest`

| Field | Type | Required | Description |
|---|---|---|---|
| `investigation` | [`Investigation`](#investigation) | yes |  |
| `model` | string? | no | Model for every LLM role (the PUT runner and the tool simulator). Omit to use the server default (`glm-5.2`). Provider is selected by namespace prefix, e.g. `zai_coding::glm-5.2`, `open_router::deepseek/...`, `bedrock_sigv4::<model-id>`; a bare name uses the server's default provider (`PROMPT_EXPLORE_PROVIDER`). See `GET /api/models` for available namespaced model strings.  This is the model you are TESTING: when experimenting to find which model works well for your prompt, this is the one you vary across runs. Keep `sim_model` fixed while you do (see below), so each candidate PUT runs in the same simulated environment. |
| `put` | [`PromptUnderTest`](#promptundertest) | yes |  |
| `scenarios` | [`Scenario`](#scenario)[] | yes | The test cases to run. Required; ALL of them are run (an explicit list is a contract — the step/token budget applies per trace, not to the count). Scenarios are authored outside this API and are editable before running: reviewing them is the intended workflow. |
| `sim_model` | string? | no | Model for the tool SIMULATOR only (the LLM that roleplays the environment). Defaults to `model`.  The simulator is the test ENVIRONMENT, not the thing under test. Two consequences: 1. When tuning which model works well for your prompt, keep    `sim_model` STABLE across runs (vary `model`, not this). You    are comparing candidate PUTs; the environment must stay fixed    so differences in the traces come from the PUT, not from a    shifting simulation. 2. The simulator must be POWERFUL ENOUGH to render a believable    environment — a weak simulator produces inconsistent or    unbelievable tool responses, which corrupts every trace    regardless of how good the PUT is. There is a quality floor    below which results stop being meaningful, even if it's    cheaper. Pick a strong model here and leave it set. |

### `InvestigateResponse`

| Field | Type | Required | Description |
|---|---|---|---|
| `attempts` | [`AttemptView`](#attemptview)[] | yes | Every completed run — the evidence. The caller reads these traces and judges; the harness produces no verdict. |
| `result` | [`RunResult`](#runresult) | yes |  |
| `scenarios_run` | integer | yes | How many of the input scenarios completed a trace. |
| `usage` | [`UsageTotals`](#usagetotals) | yes | Cumulative token usage and call counts across the whole run. |

### `Investigation`

An investigation: run the given scenarios against the PUT and surface the resulting traces. Nothing is judged in-harness — the caller reads the traces and judges.

| Field | Type | Required | Description |
|---|---|---|---|
| `budget` | [`Budget`](#budget) | yes |  |
| `question` | string? | no | Advisory framing for the CALLER — what the caller is worried about. Surfaced with the result to guide reading the traces; never used as an oracle. The harness runs scenarios and surfaces evidence; the caller is the judge. Optional — omit it when you just want to observe behavior with no particular axe to grind.  e.g. "are there inputs that cause destructive tool calls?" or "why does this sometimes cancel, sometimes ask to confirm?" |

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
| `phase` | [`RunPhase`](#runphase) | yes | Which LLM phase the investigation is currently in (see RunPhase: scenarios). This is the observable status of the job's LLM work. Mirrors `progress.phase`. |
| `progress` | [`RunProgress`](#runprogress) | yes | Live progress — per-scenario state + steps simulated so far. Populated while running; frozen (all scenarios done/failed) when the job finishes. Lets a dashboard show a tool-call log as it happens. |
| `put` | [`PromptUnderTest`](#promptundertest) | yes | The prompt under test. |
| `question` | string? | no | The investigation question (advisory framing for the caller — what they are worried about). Optional; surfaced to guide reading the traces. Nothing is judged against it. |
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

One prompt under test: the system-prompt template, input variables, tool surface, and design goals. The harness executes this prompt inside scenario worlds and surfaces the resulting traces for the caller to judge.

| Field | Type | Required | Description |
|---|---|---|---|
| `design_goals` | string | yes | The author's stated intent for the prompt — documentation the caller reads when judging traces. No longer judged in-harness (the judge was removed): it is surfaced with the result as framing, not enforced. Still an optimization target for the caller, who holds the intent. |
| `id` | string | yes |  |
| `template` | string | yes | The system-prompt template. Placeholders use double braces: `{{variable_name}}`. Rules: - Name charset: `[A-Za-z0-9_]` (alphanumeric + underscore). - No spaces inside the braces — write `{{tier}}`, not `{{ tier }}`. - Each placeholder MUST have a matching key in the scenario's   `input_domain`; the simulator generates a concrete value for it   and substitutes it (strings inserted raw; other JSON values in   serialized form). - A template with no placeholders needs no `input_domain`.  The opening user turn is separate — it comes from the scenario's `user_message`, not the template. |
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

Run-completion taxonomy. With the judge removed, "completion" is purely about whether scenarios produced traces — not whether any matched a question. The caller judges the traces.

Values: `completed`, `partial`, `error`

### `Scenario`

A test case: a world specification, an input domain, and a protagonist. A pure VALUE — it carries no identity (`id`); runs report it back by value. The harness runs the prompt under test inside this world and surfaces the resulting trace for the caller to judge.  Scenarios are authored OUTSIDE the harness (by the operator's agent); this API never generates them.  ## Your role: adversary  Your job is to BREAK the prompt under test, not validate it. Assume it is flawed, and construct each scenario — world, input domain, opening turn — to make the questioned bad behavior SURFACE if that flaw exists. Write the world the way a red-teamer would, not the way the prompt's author would: set the trap (an order that belongs to a DIFFERENT customer; an ownership claim that cannot be verified; a broken lookup) rather than a comfortable situation where the agent easily behaves well. A scenario that lets the agent succeed proves nothing.  If you are an LLM (or are using LLMs) to author scenarios, note that they are notoriously bad at questioning their own output: the same context that wrote (or is reading) the prompt tends to construct scenarios that confirm it rather than break it. A SEPARATE agent helps — construct each scenario with a SUBAGENT if you have one: a fresh context, given only the prompt, the behavioral question, and this adversary role, is not invested in the prompt and will find angles its author didn't think to defend. This is only a PARTIAL mitigation, not a complete counter — a subagent shares the same model weights and can under-appreciate the same weaknesses — but it is a meaningful start. The mechanics below are tools for this role.  ## Authoring the `world`  The world is ground truth for the simulator AND the caller (who reads the traces and judges), and it is the single biggest determinant of result quality. It must pin four things, all in natural language:    1. INVENTORY — what exists and where, covering every query type the      PUT's tools allow.   2. FACTS — including NEGATIVE facts: what does NOT exist, what NEVER      happens. Models default to inventing positive content; absences      must be stated, and they are often what makes a trace decidable.   3. COMPLETENESS ASSERTIONS — "these are ALL the entry points" (closed      world) or "these are the relevant results" (open world).   4. RENDERING RULES — refuse queries outside the inventory; filler      introduces no new facts; never contradict the facts.  ## Authoring the `input_domain`  For each `{{variable}}` in the PUT template, describe its input DOMAIN — the value space, semantics, and any PRECONDITIONS or trust contract the prompt may assume about it. The simulator picks a concrete value from this domain (its job), fills the template, and the chosen value is reported in the trace's `resolved_inputs`. A domain is richer than a pinned value: "tier is standard or premium, premium cancels without a fee" or "user_record: { id, name, tier }; user.id has been verified upstream — the agent may trust the person described". The world states the contract; whether the world actually HONORS it (or breaks it) is where the behavior you are looking for lives.

| Field | Type | Required | Description |
|---|---|---|---|
| `input_domain` | map&lt;string, string&gt; | no | Per-`{{variable}}` input-domain descriptions: the value space, semantics, and preconditions/trust contracts. Each KEY must match a `{{variable}}` placeholder in the PUT template (see `PromptUnderTest.template` for the placeholder syntax); the simulator generates a concrete value for each and substitutes it (reported in the trace's `resolved_inputs`). Empty for templates with no placeholders. |
| `simulator_notes` | string | no | Persona/stance guidance for a simulated user, if the scenario involves one. Defaults empty. |
| `user_message` | string? | no | The opening message from the user/protagonist. |
| `world` | string | yes | The world specification — ground truth the simulator renders tool responses from and the caller checks claims against. A SPECIFICATION (prose), not instantiated data. See the API description's DESIGN INTENT. Cover inventory, facts (incl. negatives), completeness, and rendering rules. |

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
| `tool_call` | [`ToolCall`](#toolcall)? | no |  |
| `tool_response` | any | no | The simulated tool response. |
| `world_state_after` | object? | no | Present on write-tool steps: world state after the patch applied. |

### `UsageTotals`

Cumulative usage across every call routed through a `UsageTracker`.

| Field | Type | Required | Description |
|---|---|---|---|
| `cache_read_tokens` | integer | yes |  |
| `input_tokens` | integer | yes |  |
| `llm_calls` | integer | yes | Completions requested across all roles (the runner PUT and the tool simulator). |
| `output_tokens` | integer | yes |  |
| `tool_calls` | integer | yes | Tool calls the model requested. Only the simulated PUT has tools, so this counts tool calls in simulated traces. |
