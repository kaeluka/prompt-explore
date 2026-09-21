# Lua investigation orchestration (experimental)

A scenario describes the world. An investigation's Lua program describes the
application being tested: prompts, models, stages, handoffs, branches and loops.
Changing the decomposition does not require changing the scenario.

Submit `workflow` instead of the single-agent `put`, `put_model`,
`put_thinking_level` and `conversation_controls` shorthand:

```json
{
  "scenario_id": "scn-example",
  "investigation": {"budget": {"max_steps_per_trace": 12, "max_tokens": 12000}},
  "workflow": {
    "lua_source": "return function(params, ctx) local a = ctx.run_agent { name = 'extract', prompt = params.extract, model = params.model, input = ctx.input }; if a.output == nil then return {delivered=false} end; local b = ctx.run_agent { name = 'review', prompt = params.review, model = params.model, input = a.output }; return b.output end",
    "params": {
      "model": "open_router::openai/gpt-4.1-nano",
      "extract": "Extract the relevant facts using the available tools.",
      "review": "Review these findings and write the final answer."
    }
  },
  "attributes": {"variant": "extract-review"}
}
```

`params` is any JSON value. No keys select models, prompts or controls by magic:
only the program interprets them. Constants in source work equally well. Omit
`lua_source` to select the default program. It reads `params.prompt`,
`params.model` and optional `params.controls`, calls one agent with `ctx.input`,
and returns its output (raising an error if the agent failed). These keys are
conventions of that program only. The
legacy single-agent request is translated to that same execution path, not a
separate runtime.

## Program contract

The Lua source chunk **returns a function** `function(params, ctx)`. Its return
value is the workflow's JSON output. A returned value is not a quality verdict.
The host records the exact source, parameters, agent inputs/outputs, tool responses,
and failures independently of what the program returns or discards.

The sandbox exposes no filesystem, network, OS, package loading or simulator
private source/world narrative. Application access to the world goes through
scenario tools. This is a different capability boundary from tool-handler Lua.
`json.null` represents JSON null; `json.array` constructs an explicitly marked
JSON array (important for empty arrays).

## `ctx`

- `ctx.input`: the scenario's opening user message.
- `ctx.resolved_inputs`: the sampled or explicitly pinned scenario input bindings.
- `ctx.run_agent(options)`: await a fresh agent conversation to completion.
- `ctx.call_tool(name, arguments)`: invoke a scenario tool directly and return its
  response JSON. No agent/model call is needed just to call a tool.

There is one simulator session, input sample, world state and workspace per
investigation. Agent messages are **not** shared: handoffs must be explicit. Tool
side effects **are** shared, including writes made directly by orchestration.
Scenario definitions and their workspace seeds remain unchanged across runs.

## Agent options

```lua
local result = ctx.run_agent {
  name = "extract",                    -- evidence label; repeats are allowed
  prompt = params.prompts.extract,     -- exact system prompt text
  model = params.models.extract.id,    -- use a provider-qualified model name
  input = ctx.input,                   -- opening user message
  tools = {"read_file"},               -- optional subset; omitted means all
  controls = {
    thinking = "high",                 -- optional, provider-dependent
    temperature = 0.2,
    max_tokens = 1024,                  -- maximum output tokens per completion
  },
  budget = {max_steps = 4, max_tokens = 4000}, -- optional invocation caps
}
```

Prompt and input handoffs are literal text, not simulator-regenerated values.
Per-invocation budgets never enlarge the remaining investigation-wide limits.
Use `GET /api/models` to discover model identifiers; availability in the catalog
is not a generation/balance check.

`run_agent` returns one table:

```lua
{
  invocation_id = 0, event_id = 0, name = "extract", model = "...",
  output = "...",             -- absent/nil without a final completion
  failure = nil,              -- or {stage="runner", error="...", invocation_id=..., event_id=...}
  stop_reason = "final_completion",
  steps_used = 2, tokens_used = 450,
  -- plus effective input/tools/controls and turn count
}
```

Ordinary agent failures are results, so Lua can branch or retry; malformed
arguments and exhausted global limits raise errors. Check `result.failure` and
`result.output == nil` explicitly. An empty final completion is the empty string,
not nil. Reaching a stage's step/token limit need not produce a final answer.
The host retains partial turns and each stage's charged cutoff completion even
when the program retries or discards the result.

## Budgets and failures

`investigation.budget` applies to the **whole program**, including every agent
invocation and direct tool call. The token cap counts all agent input/output
tokens across models; simulator tokens remain separately tracked, as before. Agent-requested sibling tool calls retain the
existing atomic-batch semantics: an accepted batch can cross the step cap; no
later model turn starts. Repeating calls or wrapping them in `pcall` cannot reset
or bypass an exhausted global budget.

`workflow.limits` bounds Lua memory, instructions, active Lua time, source/result
bytes and host calls. Limits have validated hard ceilings. Awaiting a host model
call is not Lua CPU time. The sandbox is cooperative in-process isolation, not an
OS process boundary; native operations cannot be interrupted mid-call.

Lua errors do **not** delegate orchestration to the simulator or generate a repair.
Failures retain source, parameters, completed/partial invocations, direct tool
calls and spend. Tool-handler fallback remains the scenario's existing behavior.

## Reading and comparing evidence

Prefer `GET /api/investigations/{id}/evidence`:

- `workflow.output`: what the application actually returned, not necessarily the
  last agent answer.
- `workflow.invocations`: exact prompts, models, inputs, exposed tools, controls,
  effective stage budgets, outputs, stop reasons, failures and zero-based `[turn_start, turn_end)` ranges
  into the flat `turns` array.
- `workflow.tool_calls`: direct calls, actual responses and simulation provenance.
  Agent invocations and direct calls share monotonic `event_id` values so their
  interleaving is explicit. Records marked `running` have not completed yet.
- `workflow.source` and `workflow.params`: the orchestration that ran.
- `workflow_program`: submitted custom configuration, retained even if execution
  never began. Live/failed runs retain partial `workflow` evidence.

Caller assessments and grades still apply to the complete investigation. No
in-harness judge is added. Record your judgments with PATCH and compare using
`POST /api/frontier`. `workflow_hash` pins custom source, parameters and limits;
custom programs have no single `put_model`/`put_thinking` attribute. Their
`prompt_hash` identifies the workflow for the existing default grouping. Use
explicit `variant` attributes when comparing decompositions. Multi-model cost is
summed from actual per-model usage; a missing model price leaves cost unavailable
rather than applying one model's price to all tokens.

Start with the same scenario and explicit `resolved_inputs` for matched runs.
Sequential stages are supported; concurrency, resume and external application
execution are outside this initial implementation.
