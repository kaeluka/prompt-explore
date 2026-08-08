# prompt-explore API

Property-based testing for agent behavior. Job-based API: start an investigation, poll for the result, apply proposals.

Version: `0.1.0` — generated from `openapi.json`; do not edit by hand (see `scripts/dump-openapi.sh`).

## Endpoints

### `GET /`

Serve the web UI.

| Status | Response |
|---|---|
| `200` | Web UI (HTML) |

### `POST /api/apply`

Apply a proposal: the LLM rewrites the target field (template, or design goals for goal_revision), and a deterministic word-level diff is returned for review alongside the updated prompt set.

Body: [`ApplyRequest`](#applyrequest)

| Field | Type | Required | Description |
|---|---|---|---|
| `proposal` | [`Proposal`](#proposal) | yes |  |
| `psut` | [`PromptsUnderTest`](#promptsundertest) | yes |  |
| `target_put` | string? | no | Id of the prompt under test to apply the proposal to. Defaults to the first. |


| Status | Response |
|---|---|
| `200` | Updated prompt set plus template/goals diffs: [`ApplyResponse`](#applyresponse) |
| `400` | Unknown target prompt |
| `500` | LLM apply failed |

### `POST /api/investigations`

Start an investigation. Runs in the background; poll the returned id.

Body: [`InvestigateRequest`](#investigaterequest)

| Field | Type | Required | Description |
|---|---|---|---|
| `investigation` | [`Investigation`](#investigation) | yes |  |
| `psut` | [`PromptsUnderTest`](#promptsundertest) | yes |  |


| Status | Response |
|---|---|
| `202` | Investigation job created: [`JobCreated`](#jobcreated) |

### `GET /api/investigations/{id}`

Poll an investigation job. `status: done` includes the full result; `running` means keep polling; `failed` carries an error message.

| Parameter | In | Type | Description |
|---|---|---|---|
| `id` | path | string | Job id returned by POST /api/investigations |

| Status | Response |
|---|---|
| `200` | Job status (and result, when done): [`JobView`](#jobview) |
| `404` | Unknown job id |

## Schemas

### `ApplyRequest`

| Field | Type | Required | Description |
|---|---|---|---|
| `proposal` | [`Proposal`](#proposal) | yes |  |
| `psut` | [`PromptsUnderTest`](#promptsundertest) | yes |  |
| `target_put` | string? | no | Id of the prompt under test to apply the proposal to. Defaults to the first. |

### `ApplyResponse`

| Field | Type | Required | Description |
|---|---|---|---|
| `goals_diff` | [`DiffPart`](#diffpart)[] | yes |  |
| `psut` | [`PromptsUnderTest`](#promptsundertest) | yes |  |
| `template_diff` | [`DiffPart`](#diffpart)[] | yes |  |

### `AttemptView`

| Field | Type | Required | Description |
|---|---|---|---|
| `final_world_state` | map&lt;string, any&gt; | yes | World state at the end of the trace (after all applied patches). |
| `hypothesis_id` | string | yes |  |
| `matched` | boolean | yes |  |
| `steps` | [`TraceStep`](#tracestep)[] | yes | Structured steps, rendered as HTML by the UI. |
| `user_message` | string? | no |  |
| `verdict_confidence` | number? | no |  |
| `verdict_rationale` | string | yes |  |

### `Attribution`

| Field | Type | Required | Description |
|---|---|---|---|
| `evidence` | string | yes | e.g. ablation summary |
| `instruction_spans` | string[] | yes |  |

### `Budget`

| Field | Type | Required | Description |
|---|---|---|---|
| `max_scenarios` | integer | yes |  |
| `max_steps_per_trace` | integer | yes |  |
| `max_tokens` | integer? | no |  |

### `DiffPart`

**Variant `equal`**

| Field | Type | Required | Description |
|---|---|---|---|
| `tag` | `equal` | yes |  |
| `value` | string | yes |  |

**Variant `insert`**

| Field | Type | Required | Description |
|---|---|---|---|
| `tag` | `insert` | yes |  |
| `value` | string | yes |  |

**Variant `delete`**

| Field | Type | Required | Description |
|---|---|---|---|
| `tag` | `delete` | yes |  |
| `value` | string | yes |  |

### `InvestigateRequest`

| Field | Type | Required | Description |
|---|---|---|---|
| `investigation` | [`Investigation`](#investigation) | yes |  |
| `psut` | [`PromptsUnderTest`](#promptsundertest) | yes |  |

### `InvestigateResponse`

| Field | Type | Required | Description |
|---|---|---|---|
| `attempts` | [`AttemptView`](#attemptview)[] | yes | Every completed run — the evidence behind a negative result. |
| `result` | [`RunResult`](#runresult) | yes |  |
| `scenarios_generated` | integer | yes |  |
| `witness_user_message` | string? | no | The opening user message of the witness scenario, so the UI can show the full conversation (the trace steps start with the agent's first reply). |

### `Investigation`

| Field | Type | Required | Description |
|---|---|---|---|
| `budget` | [`Budget`](#budget) | yes |  |
| `initial_state` | string? | no | Optional user-specified starting environment state, in natural language (e.g. "cancel_order is broken and returns E_CONN; order 123 is already shipped"). It is NOT compiled or enforced: it flows into scenario building, the simulator notes, and the judge's scenario context as-is. Free-text — pasting a previous run's returned `final_state` JSON works fine. |
| `question` | string | yes | The mandatory question, e.g. "are there inputs that cause destructive tool calls?" or "why does this sometimes cancel, sometimes ask to confirm?" |
| `target_put` | string | yes | Id of the prompt under test — a run executes exactly one prompt. |

### `JobCreated`

| Field | Type | Required | Description |
|---|---|---|---|
| `id` | string | yes |  |

### `JobStatus`

Values: `running`, `done`, `failed`

### `JobView`

| Field | Type | Required | Description |
|---|---|---|---|
| `error` | string? | no |  |
| `result` | [`InvestigateResponse`](#investigateresponse)? | no |  |
| `status` | [`JobStatus`](#jobstatus) | yes |  |

### `PromptUnderTest`

One prompt under test: template, input variables, tool surface, and (mandatory) design goals.

| Field | Type | Required | Description |
|---|---|---|---|
| `design_goals` | string | yes | MANDATORY. The yardstick for judging behavior; also itself an optimization target (flagged via `ProposalKind::GoalRevision`). |
| `id` | string | yes |  |
| `input_vars` | map&lt;string, [`VarSpec`](#varspec)&gt; | yes |  |
| `template` | string | yes |  |
| `tools` | [`ToolSchema`](#toolschema)[] | yes | This prompt's tool surface, exactly as the model sees it. Empty = no tool loop (but intent lives in `design_goals`, not here). |

### `PromptsUnderTest`

The prompts under test (PsUT): a set of agent prompts, how they connect, and optional pipeline-level design goals.

| Field | Type | Required | Description |
|---|---|---|---|
| `design_goals` | string? | no | Pipeline-level constraints, e.g. "never promise refunds above $500". |
| `prompts` | [`PromptUnderTest`](#promptundertest)[] | yes |  |
| `topology` | string | yes | NL description of how the prompts connect (pipeline, usually a DAG). Used for hypothesis generation and cross-prompt proposals; NOT executed — a run targets one prompt at a time. |

### `Proposal`

| Field | Type | Required | Description |
|---|---|---|---|
| `addresses` | string[] | yes | Instruction spans this proposal addresses. |
| `confidence_note` | string | yes | Must state explicitly that the proposal is unverified. |
| `content` | string | yes |  |
| `kind` | [`ProposalKind`](#proposalkind) | yes |  |

### `ProposalKind`

Values: `reword`, `split`, `merge`, `data_transform`, `goal_revision`

### `RunResult`

| Field | Type | Required | Description |
|---|---|---|---|
| `final_state` | object? | no | The latest world state: from the witness trace when one was found, otherwise from the last completed attempt. Feed it back as `initial_state` of a follow-up investigation to chain runs. |
| `incidental_findings` | string[] | yes | Goal violations found incidentally during the search. |
| `proposals` | [`Proposal`](#proposal)[] | yes | May be non-empty even on negative results (defensive hardening). Always unverified; the user owns everything after the run. |
| `scenarios_tried` | integer | yes |  |
| `status` | [`RunStatus`](#runstatus) | yes |  |
| `strategies_tried` | string[] | yes | Hypothesis summaries — shown on negative results. |
| `witness` | [`Witness`](#witness)? | no |  |

### `RunStatus`

Values: `witness_found`, `no_witness_within_budget`, `error`

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
| `example_responses` | string[] | no | Optional realism anchors for the simulator LLM. |
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
| `model_output` | string | yes |  |
| `tool_call` | [`ToolCall`](#toolcall)? | no |  |
| `tool_response` | any | no |  |
| `world_state_after` | object? | no | Present on write-tool steps. |

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
| `matched` | boolean | yes | Does this trace satisfy the predicate (∧ design_goals)? |
| `matched_step_indices` | integer[] | yes | Where in the trace the match happened. |
| `rationale` | string | yes |  |

### `Witness`

| Field | Type | Required | Description |
|---|---|---|---|
| `attribution` | [`Attribution`](#attribution) | yes |  |
| `traces` | [`Trace`](#trace)[] | yes | 1 trace for existential questions, 2 for divergence. |
