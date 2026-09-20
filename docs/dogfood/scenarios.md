# Dogfood: reusable scenarios and simulation probes

Live run on 2026-09-20 against a scratch server (release build, OpenRouter for
both roles), driving the new workflow over HTTP exactly as a caller would:
register a world once, probe it, fix it, run an investigation, notice the world
was pinned, fork, and cascade-delete.

## What the workflow did

**Registered once.** `POST /api/scenarios` (multipart: a `request` JSON part and
a `workspace` zip part) returned
`{"id":"scn-707ea681150a","revision":1,"definition_hash":"488d30b3…","workspace_hash":"6d3b7ac…"}`.
Three tools: `list_dir` and `grep` with `lua_source`, `read_file` without.

**Probed before spending anything.** Four calls in one submission came back with
per-call provenance:

| call | response | provenance |
| -- | -- | -- |
| `list_dir {"path":"."}` | the two inventory entries | `list_dir` **computed** (no model call) |
| `grep {"pattern":"println"}` | the real match | `grep` **fallback** ("render search results from the world") |
| `read_file {"nope":1}` | `error: invalid arguments: Additional properties are not allowed` | no Lua attempt, no model call |
| `list_dir {"path":"tests"}` | `{"error":"not found"}` | `list_dir` **computed** |

**Edited while unreferenced, then re-probed.** `PATCH` with
`expected_revision: 1` → revision 2 and a new `definition_hash`; the workspace
hash was unchanged. A second probe (revision 2) exercised the new `grep`
implementation: the common pattern computed, and a pattern outside its
implemented subset delegated — the intended compute/delegate split, visible per
call. The first probe still reports `scenario_revision: 1`, so the two
revisions are distinguishable after the fact.

**Investigated, then met the pin.** `POST /api/investigations` with
`scenario_id` + `scenario_revision: 2` produced a trace whose evidence reports
`scenario_id`, `scenario_revision: 2`, `scenario_definition_hash`, and
`implementations: [list_dir b9bf45aa, grep 14b99d41]`; attributes carried
`scenario_revision=2`, `scenario_hash`, and `simulation_backend=lua`. A `PATCH`
then failed with
`scenario 'scn-707ea681150a' is pinned by 1 investigation(s) (807c3cab-…)`.

**Forked with a correction.** `POST /api/scenarios/{id}/fork` with a correction
note returned a new editable id at revision 1 with the same workspace hash (no
re-upload) and recorded
`correction: {scenario_id, revision: 2, reason: "grep rendered an off-schema extra key; tighten the contract"}`.

**Deleted safely.** Without `cascade`, deletion refused and named the dependent.
With `cascade=true` it removed the scenario, the dependent investigation and its
two probes, reported all of them, and left the fork intact (`404` for the job,
`200` for the fork).

**Probed from the dashboard.** The scenarios panel listed the fork, showed the
definition plus both implementations, ran a two-call probe from the form, and
rendered each response with its `supplied Lua · <tool> · <hash>` chip and
outcome. No console errors.

## What the run actually found (the evidence, not a verdict)

* **The PUT never listed the root.** It called `grep` once with
  `{"pattern":"print(\"Hello, World!\")"}` — a pattern invented from the
  question — got no matches, and asked the user what the greeting was. That is a
  real behavior finding, and the trace shows it plainly; the harness did not
  hide it behind a plausible final answer.
* **The delegated response drifted off-schema.** The world declares
  `{matches, truncated}`; the rendered response added a `pattern` key. The
  simulation was "good enough" to be believable and wrong in exactly the way the
  caller must catch — which is why the follow-up fork exists.
* **Compute/delegate split saves real calls.** The `list_dir` calls and the
  implemented `grep` pattern cost zero simulator completions; only the two
  delegated calls (one probe, one investigation) reached the model.

## Harness bugs this dogfood found and fixed

1. **Probe usage raced its own finalization.** The probe's `status` flipped to
   `done` inside the run, while usage/cost were stored by the surrounding task
   afterwards, so an immediate poll reported `usage: null`. Now the probe keeps
   its usage tracker and the read handlers refresh totals through it, so a
   single poll is enough.
2. **A finished probe's calls were hidden.** The result details rendered closed
   once a probe stopped, so the caller saw a summary line and had to expand it.
   The newest probe is now open by default.

## Residual caveats

* The simulator LLM can still render off-schema responses (finding above); the
  fix is to sharpen the tool contract or the world and fork, which is what a
  caller would do with any LLM-rendered tool.
* A delegation that then fails loses that call's Lua record: the call is
  reported as unrenderable with the provider error, not as "Lua declined, then
  the model failed". The completed calls in the same probe are retained.
* Scenario and probe state is in memory like the job store: restart loses it.
  `GET /api/scenarios/{id}/workspace` exports the initial inventory so the world
  can be re-registered.
