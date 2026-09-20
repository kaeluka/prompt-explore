# Reusable scenarios and caller-authored simulation

An investigation used to be self-contained: it carried its own world, its own
workspace upload, its own tool contracts, and it let the simulator LLM author a
Lua program during the run. That repeated the expensive parts once per
investigation (upload, authoring), made two runs of "the same" scenario differ
in ways nobody chose, and turned a slow authoring loop into a fatal
preparation phase.

The definition is now a separate, reusable value.

## Three concepts

| Concept | What it owns |
| -- | -- |
| **Scenario definition** | `world`, `input_domain`, `user_message`, `simulator_notes`, `tools[]` (contract + optional `lua_source`), `simulation` settings, and the initial workspace |
| **Run** | One reference to a scenario revision, one resolved input sample, one isolated workspace overlay, one simulator conversation |
| **Investigation** | A PUT driving exactly one run, with its own budget, reason, grades and attributes |

Reuse the DEFINITION, never the mutable session. Every run clones the shared,
immutable workspace seed and keeps its own overlay, world state and simulator
conversation, so a probe that writes a file cannot change what the next
investigation starts from.

## Identity, pinning, correction

A stored scenario has an `id`, a monotonically increasing `revision`, and a
`definition_hash` over everything that can change an execution (narrative,
domains, contracts, Lua sources, simulator settings) plus a `workspace_hash`
over the initial files. Display metadata (`label`) is deliberately outside the
hash.

The lifecycle rules live in `core::scenario::ScenarioStore`, not in HTTP
handlers:

* **Editable exactly while unreferenced.** No publish step: accepting an
  investigation pins the scenario. Finished, failed and budget-capped runs keep
  pinning it, because the traces they produced must still describe what ran.
* **Stale edits are refused.** `PATCH` requires the revision the caller read.
* **Deleting the last referencing investigation unlocks it.** The revision keeps
  advancing, so an old probe result cannot be confused with new contents.
* **Deletion is refused while anything is running** (a run cannot be cancelled),
  and refused without `cascade=true` when finished investigations still depend
  on it. Cascade is all-or-nothing and reports what it removed.
* **Forking shares the immutable workspace seed** — no re-upload, no
  re-decompression — and may record a `correction` naming the predecessor and
  explaining what changed. That link is descriptive history: it neither locks
  nor invalidates the predecessor, and it survives its deletion.

Every investigation and evidence read reports `scenario_id`,
`scenario_revision` and `scenario_definition_hash`. Discovering a broken
simulation therefore yields a clean diff: fix the definition, fork it, and see
exactly which traces ran the old revision.

Probes never pin: they snapshot the revision they test and report it.

## Testing a simulation before spending investigations

`POST /api/scenarios/{id}/simulations` takes an ordered list of tool calls and
executes them through the SAME engine an investigation uses
(`core::simulate::SimEngine`): the same argument validation, the same sandbox,
the same delegation to the simulator LLM, the same workspace rollback, the same
world-state patches. Calls run sequentially in one session from a fresh
snapshot, so read-after-write consistency is testable; a new submission starts
fresh.

Each call reports the response, the Lua provenance (`computed`, `fallback`,
`error` — with the tool name and the exact `source_hash`, plus any discarded
writes), the simulator's workspace operations, its reasoning, and the elapsed
time. A delegation that the LLM cannot render fails that CALL and keeps the
completed calls as evidence.

The point is that a probe is evidence about what an investigation will do, not
evidence about a second, differently-behaving simulator. A caller can therefore
develop and test a world entirely over HTTP — **no local Lua toolchain, no
provider credentials** when the scenario's inputs are declared explicitly or
absent and the calls are fully implemented in Lua.

## Caller-authored Lua

The harness no longer generates, repairs or rewrites code. That removed:

* the preparation phase (`preparing_tools`) and its timing,
* the generated fallback module and the `.prompt-explore/tools.lua` artifact,
* the instruction that let the simulator specialize handlers during a fallback,
* `SimulationProgram`/`ProgramRevision` from the model (evidence now reports the
  supplied `implementations` and each exchange's `source_hash`).

A tool's `lua_source` is a chunk that RETURNS a handler:

```lua
return function(args, ctx)
  return { response = { ... }, state_patch = { ... } } -- state_patch: write tools only
end
```

The sandbox is unchanged: a fresh VM per invocation, four workspace
capabilities, bounded memory/instructions/duration/conversions, no host IO,
clock or randomness, staged writes that are discarded unless the handler
computes a result. `PleaseSimulateException("reason")` delegates that one input
to the simulator LLM; a missing implementation does too, without any VM work.

`lua_source` is simulator-private: it never appears in the tool contract the
prompt under test sees, and it may encode ground truth the PUT is supposed to
discover. Supplying it is not a fidelity claim — code that executed can still be
wrong, which is why every exchange keeps both the response and the provenance.

## Simulator settings belong to the scenario

`sim_model`, `sim_thinking_level`, sampling, repair budgets, workspace bounds
and Lua limits live on the scenario definition. The environment is part of the
test case: testing a simulation under different settings than an investigation
runs it under should be an explicit edit or fork, never an invisible per-run
override. The investigation keeps only PUT-side controls.

## What this does not do

`done` still means a trace was recorded, not that the simulation was faithful,
and there is still no judge. A frozen implementation turns a noisy confound (a
freshly generated program) into a consistent one, which is easier to see and
easier to fix — but the caller still reads the responses against the narrative.

Storage stays in memory, like the job store: registration is reuse within a
server lifetime, not durability. `GET /api/scenarios/{id}/workspace` exports the
initial inventory because a hash alone is not a reproducible workspace.

## Caller-model dogfood

Probe the spec like a prompt (see AGENTS.md). Before/after, the same three
probe questions: *"I want to test my world's grep handler before running 20
investigations — walk me through your calls"*, *"my scenario is pinned and my
handler is wrong; what now?"*, and *"the run says done but the responses look
fabricated; what do I read?"*. The first must answer `POST
/api/scenarios/{id}/simulations`; the second must answer fork + the documented
revision fields; the third must answer evidence + `lua_execution` +
`workspace_ops`. Record stumbles as spec bugs and re-probe after fixing them.
