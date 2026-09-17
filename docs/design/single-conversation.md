# One investigation, one conversation

An investigation is the unit of execution, evidence, grading, and lifecycle.
It takes one authored scenario, one PUT, resolved model/conversation controls,
and an optional workspace seed. It produces one conversation or a failure.
Several investigations may run concurrently; grouping is external to execution.

## API contract

- POST requires `scenario`, an object. `scenarios`, even a one-element array,
  is rejected; there is no compatibility alias or sample-count field.
- GET embeds the authored `scenario` by value, beside the PUT and provenance.
- `progress` is flat: `phase`, `turns`, `resolved_inputs`, optional
  `simulation_program`, and optional `user_message`. Job `phase` mirrors it:
  `resolving_inputs`, `preparing_tools`, or `put_loop`.
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

Repeat a scenario by creating separate investigations. Each repetition resolves
inputs and simulates afresh, including Lua preparation if enabled. This measures
variation of the whole experiment, not exclusively PUT randomness with fixed
inputs and tool responses. The caller judges both behavior and simulation quality.

A future multi-submit convenience could accept one upload and create N ordinary
investigations sharing the immutable workspace seed. It should return their IDs,
not introduce nested samples or another grading boundary. It is not built now.

## Workspace trade-off

Within a Runner, workspace clones share an immutable seed and isolate mutations
in private overlays. Independent HTTP uploads are separately decompressed and
are not deduplicated by workspace hash. Repeated large uploads therefore add
memory and decompression costs while runs overlap (default decompressed limit:
500 MiB per upload). Different workspaces cannot benefit from seed sharing anyway.

We accept this cost for a simpler contract in an LLM-latency-dominated workload.
Workspace handles, persistence, and cross-job deduplication require separate
justification; no new resource lifecycle is introduced speculatively.

## Migration

Replace `scenarios: [s]` with `scenario: s`. Split multi-scenario submissions
into independent requests, reusing desired custom attributes. Poll each ID.
Read `result.trace` instead of `result.attempts[0]`, `result.failure` instead of
`result.result.failures`, and flat `progress` instead of `progress.scenarios[0]`.
A failed conversation has job status `failed`, not a successful batch wrapper.
The optional workspace multipart part and grades/attributes PATCH shapes stay
the same. Models, budgets, retries, and simulator prompt semantics are unchanged.

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
