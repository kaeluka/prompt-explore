# Multi-dimensional prompt optimization: grades + Pareto frontier

Status: implemented (v0). This doc records the design decisions and their
rationale, as agreed between the operator and the implementing agent.

## What this is

Support for the CALLER doing multi-dimensional optimization of a single
prompt. Optimizing a prompt is never only about correctness: the caller
also cares about cost (measured by the harness for free) and about "soft"
properties — tone of voice, self-containedness, repeatability, … — which
only the caller can judge.

The feature has two halves:

1. **Grades** — the caller PATCHes an investigation with numeric grades
   on caller-chosen axes. The harness stores them and never interprets
   them.
2. **Frontier** — the caller submits a list of investigation ids plus a
   list of axes (any mix of graded and reserved/measured axes, with a
   direction per axis); the harness resolves every (investigation, axis)
   value, computes Pareto dominance, and returns JSON points or an SVG
   scatter plot.

The harness's philosophy is unchanged: **the caller is the judge.** The
harness records the caller's judgment and does arithmetic on it — that is
deterministic bookkeeping (like diffs and budget counting), not semantic
work. Nothing is graded, ranked, or interpreted in-harness beyond Pareto
dominance over caller-supplied numbers.

## The load-bearing distinction: measured vs judged axes

- **Measured (reserved) axes** are harness-computed from run data:
  token usage, estimated USD cost, steps-per-trace statistics. Their
  better-direction is baked in and known. They can never be PATCHed.
- **Judged (graded) axes** are caller-PATCHed scalars on free-form axis
  names. The harness stores, merges, and computes dominance over them.
  Their direction is supplied per-request by the caller (`better:
  "lower" | "higher"`), NOT stored — direction only matters at
  dominance/plot time, and storing it would create axis-registration
  machinery (first-PATCH-wins? conflicting declarations?) for zero gain.

A single global convention ("higher is better") was considered and
rejected: the measured axes already break it (`put_cost_usd` is lower-is-
better, `put_cache_read_tokens` is higher-is-better), so direction is
per-axis by request. Dominance normalizes internally: each value is
negated on lower-is-better axes, then standard componentwise dominance.

### Reserved axes (v0)

| axis | better | source |
|---|---|---|
| `put_input_tokens`, `sim_input_tokens` | lower | `UsageByRole` totals |
| `put_output_tokens`, `sim_output_tokens` | lower | `UsageByRole` totals |
| `put_cache_read_tokens`, `sim_cache_read_tokens` | higher | cached input is cheaper input |
| `put_cost_usd`, `sim_cost_usd` | lower | catalog pricing; **absent when the model is unpriced** |
| `steps_per_trace_avg`, `_min`, `_max`, `_stdev` | lower | per-trace step counts (completed attempts only) |

Naming rule: `<metric>_<statistic>`, so future per-trace normalizations
(`put_output_tokens_avg`) slot in without renames. A "step" reuses the
`max_steps_per_trace` definition: one tool call OR one final completion.
`stdev` is population stdev (÷N; 0.0 at N=1) so it is well-defined for
every corpus and never opens an undefined-value path in the error
contract.

Comparability caveat (surfaced, not enforced): usage totals are only
comparable across a fixed scenario corpus and budget; the frontier
endpoint happily plots across differing corpora — the caller owns that
judgment.

## Grades (PATCH)

```
PATCH /api/investigations/{id}
{ "grades": { "tone_of_voice": 0.8, "stale_axis": null } }
```

- Merge per axis; `null` deletes; response echoes the full updated map.
- Axis names: `^[a-z][a-z0-9_]{0,63}$` (allow-pattern, not deny-list).
- Values: finite JSON numbers. NaN/Infinity are not expressible in JSON,
  but exponents like `1e999` parse to infinity — rejected with a 400
  that names the axis.
- A reserved axis name in a PATCH is a 400 naming the collision and the
  reserved direction.
- Grading is allowed on any job status (live-tagging while running is
  fine; a grade on a failed job is the caller's prerogative).
- No range enforcement: the caller may choose 0..1, 1..5, or anything;
  dominance needs comparability, not range.

## Frontier (POST)

GET-with-body was considered and rejected: browsers' `fetch()` throws on
GET bodies, so the web UI could never call it. The endpoint is POST.

```
POST /api/frontier?format=json|svg    (default json)
{
  "investigations": [
    "uuid1",
    { "id": "uuid2", "label": "v3-tone-pass", "color": "#e07a5f" }
  ],
  "axes": [
    { "name": "put_cost_usd", "better": "lower" },
    { "name": "tone_of_voice", "better": "higher" }
  ]
}
```

- `investigations`: bare id strings or `{id, label?, color?}` objects
  (untagged). **Uniqueness enforced** — duplicate ids are a 422 naming
  the duplicate (the caller's explicit micro-decision).
- `axes`: exactly 2 for `format=svg`; any count ≥ 1 for `format=json`.
  The 2-axis limit is a v0 RENDERING constraint only — the dominance
  computation is N-dimensional from day one, so parallel coordinates or
  other renderers slot in without touching the math.
- Labels: `^[A-Za-z0-9_-]{1,64}$` (allow-pattern; injection defense).
  Colors: `^#[0-9a-fA-F]{6}$`. Defaults: label = `put.id` (deduplicated
  with `#2`, `#3`… when several points share one, since default labels
  come from the same PUT lineage) else uuid prefix; color = deterministic
  palette by index.
- Requesting a reserved axis with a `better` that contradicts its
  measured direction is a 422 (`direction_conflict`) with the fix named.
- Only `done` jobs can be points: running jobs have incomplete usage,
  failed jobs have no judgeable traces. Both are typed problems.

### The error contract

Every fixable problem comes back in ONE 422 envelope with typed reasons
and a `detail` that names the fix — including the exact PATCH to make:

```json
{ "error": "frontier_request_invalid",
  "problems": [
    { "investigation": "uuid3", "axis": "tone_of_voice",
      "reason": "no_grade",
      "detail": "no caller grade named 'tone_of_voice'; PATCH /api/investigations/uuid3 with {\"grades\":{\"tone_of_voice\": <number>}} (higher = better on your scale); graded axes on this investigation: clarity, brevity" },
    ...
  ] }
```

Reasons: `unknown_investigation`, `duplicate_investigation`, `job_running`,
`job_failed`, `no_grade`, `axis_absent` (measured axis with no value —
e.g. unpriced model, or no completed traces), `direction_conflict`,
`bad_axis_name`, `duplicate_axis`, `axis_arity` (svg ≠ 2), `bad_label`,
`bad_color`, `empty_investigations`, `empty_axes`.

`no_grade` details list the axes already graded (typo detection) and the
reserved axis vocabulary (typo detection for measured names).

### JSON response

```json
{ "points": [
    { "investigation": "uuid2", "label": "v3-tone-pass", "color": "#e07a5f",
      "values": { "put_cost_usd": 0.42, "tone_of_voice": 0.8 },
      "on_frontier": true, "dominated_by": [] },
    { "investigation": "uuid1", "label": "cancel-bot", "color": "#1f77b4",
      "values": { "put_cost_usd": 0.31, "tone_of_voice": 0.5 },
      "on_frontier": false, "dominated_by": ["uuid2"] } ] }
```

`dominated_by` uses uuids (labels are not unique by design). Ties
dominate nothing: equal points are both on the frontier.

### SVG

Hand-rolled (no charting dependency): scatter + stepped frontier
polyline through the non-dominated set, tick marks via nice-numbers, and
**orientation inversion so up-and-right is always better** regardless of
which directions the axes have — the frontier always reads as an
upper-right envelope. All text is XML-escaped; labels/colors are
allow-pattern-validated before ever reaching the renderer (defense in
depth). Output is deterministic (golden-tested).

## Where things live

- `core/src/frontier/` — request/response types (utoipa-annotated),
  axis vocabulary, validation, dominance, SVG generation. Pure and
  golden-tested; no server dependency.
- `server/` — thin: `grades` map on the job store, the PATCH and
  frontier handlers, snapshot assembly (jobs → `InvestigationSnapshot`).
- Durability: NONE, deliberately. The job store is in memory (a
  documented, conscious limitation); the frontier is computed from live
  jobs on demand. The CALLER's session holds the set ("these five uuids
  are my campaign") because it created them — ad-hoc caller-owned sets,
  zero grouping/lineage machinery in the harness. If a durable
  optimization ledger is ever wanted, that is its own scoped decision.

## What this is not

- No verdicts: grades are the caller's judgment recorded, never checked
  against traces or questions.
- No persistence: restart loses jobs and grades (caller re-runs or
  re-grades).
- No aggregation semantics: grading per-scenario ("scenario_3_correct")
  is just an axis name; aggregating per-scenario grades into a
  prompt-level number stays the caller's job.
