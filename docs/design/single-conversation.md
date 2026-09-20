# One investigation, one conversation

An investigation is the unit of execution, evidence, grading, and lifecycle.
It takes one authored scenario, one PUT, resolved model/conversation controls,
and an optional workspace seed. It produces one conversation or a failure.
Several investigations may run concurrently; grouping is external to execution.

## API contract

- POST requires `scenario_id`, naming a stored scenario definition (see
  `docs/design/scenarios.md`), plus the PUT and the investigation's own controls.
  An inline `scenario`/`scenarios`, a per-run workspace upload, `sim_model`,
  `sim_thinking_level`, the simulator keys of `conversation_controls`, and
  `put.tools` are all rejected with migration guidance: the world, its tool
  surface, its simulation settings and its workspace belong to the scenario.
- GET embeds the pinned narrative by value, beside the PUT, the
  `scenario_id`/`scenario_revision`/`scenario_definition_hash` it came from, and
  the supplied Lua implementations.
- `progress` is flat: `phase`, `turns`, `resolved_inputs`, optional
  `implementations`, and optional `user_message`. Job `phase` mirrors it:
  `resolving_inputs` or `put_loop`.
- A finished job has `result: {trace, failure, usage}`. Success has a trace and
  no failure; failure has a structured `{stage, error}` and no completed trace.
  There is no partial-batch status, result-within-result, or attempts array.
- Failure does not erase evidence: completed turns, resolved inputs, and Lua
  setup/source remain available in `progress`. If a later sibling tool call
  fails, the final progress turn retains the successful exchanges from that
  completion; it is not a fabricated complete trace. Usage is recorded on
  failure too.
- Grades and measurements belong to exactly one conversation. Group means use
  the same complete cohort for every requested axis, with equal weight per job.

A trace remains multi-turn: a PUT completion can request several tool calls,
which are simulated in provider order and retained together in that turn.
Singular investigation does not mean a single model completion or tool call.

## Grouping and repetition

Use attributes to group independent investigations. The UI's card groups and
Pareto points use the same selected grouping keys, including null groups for
missing values. Hover/focus links a plotted group to its cards; activation
scrolls to that group. GET-list attribute filters are browsing conveniences,
not frontier selection: every stored investigation remains a candidate.

Repeat a scenario by creating separate investigations that reference the SAME
scenario revision. Each repetition resolves inputs and simulates afresh, with the
same workspace seed and the same supplied implementations — so what varies is the
PUT (and the simulator LLM's rendering), not a freshly generated program. Pass
`resolved_inputs` to pin one input sample and isolate that further. The caller
judges both behavior and simulation quality.

Multi-submit remains unbuilt: submit one investigation per repetition, or script
the loop. A convenience that accepts one scenario id and creates N ordinary
investigations should return their IDs, not introduce nested samples or another
grading boundary.

## Workspace trade-off

A workspace is uploaded ONCE, with the scenario. Every run clones the immutable
seed (shared by `Arc`) and isolates its mutations in a private overlay, so
repeated investigations pay neither the upload nor the decompression again.
Forking a scenario shares the same seed. Two scenarios with identical contents
still hold separate seeds: content-addressed deduplication across scenarios was
not built, because it would add a resource lifecycle for no observed need.

Uploaded archives are decompressed in memory with the same hard caps (default:
50 MiB compressed, 500 MiB decompressed) and are never written to disk.

## Migration

Replace inline `scenario` with `scenario_id`: register the world once
(`POST /api/scenarios`, multipart `request` JSON + optional `workspace` archive),
moving the tool contracts, the simulator settings and any Lua implementation into
it. Then submit one investigation per PUT. Read `result.trace` instead of
`result.attempts[0]`, `result.failure` instead of `result.result.failures`, and
flat `progress` instead of `progress.scenarios[0]`. A failed conversation has job
status `failed`, not a successful batch wrapper. Grades, attributes and
assessment PATCH shapes are unchanged.

## Caller-model dogfood

Fed the complete before/after OpenAPI documents to the same Luna low-thinking
PUT via prompt-explore, with an identical operational probe: submit three
workspaces concurrently, inspect Lua preparation, recover evidence after a
simulator failure, grade conversations, filter a campaign, and repeat five times.

Before: the caller located errors at `result.result.failures` but incorrectly
looked for the failed conversation's successful turns in completed `attempts`.
It used five scenario-array entries in one job for repetitions, introducing the
batch-level grading boundary we wanted to remove. Campaign listing required a
client-side filter.

After: it used singular `scenario`, flat `progress`, `result.trace` on success,
and `result.failure` plus `progress.turns` on failure. It explicitly warned not
to read the absent completed trace for a failed job. It found the documented
attribute query, distinguished it from frontier candidacy, and launched five
independent investigations without inventing sample controls or workspace handles.

This is evidence of improved API navigation, not a guarantee of caller behavior:
its campaign-wide grouping also includes earlier matching jobs, so an operator
still needs to choose attributes appropriate to the cohort they intend. The
harness does not infer experimental intent or judge comparability.
