# Live grouped Pareto frontier

Investigations are evidence; grades are the caller's judgment recorded. The
frontier does deterministic bookkeeping over those numbers, not judging.

## The simple model

Every investigation in server memory is a candidate. Pick **grouping attribute
names** and **axes**. Each unique combination of attribute values becomes a point.
Coordinates are arithmetic means over completed investigations having a value
on **every** requested axis. Each included investigation has equal weight.
No frontier candidate selection, filter language, or weighting framework.
The GET-list attribute filter only changes browsing; it never changes this
cohort. Delete investigations that should not contribute.

This supports one point for a prompt/model configuration evaluated across
several workspace uploads. Workspace identity need not be a grouping key.

Each investigation runs one scenario and produces one conversation. Means of
investigation metrics therefore weight conversations equally. Repeat a scenario
by submitting separate investigations; no nested samples are required. Benchmark
coverage, repeated-run weighting, grading scales, and simulator comparability
remain the caller's responsibility.

## Attributes

Attributes are string-valued key/value pairs, exposed on investigation views and
summaries through the literal `attributes` field. Keys use
`^[a-z][a-z0-9_]{0,63}$`. There is no `tags` compatibility alias: this feature
was unreleased when renamed, and unknown request fields are rejected.

System-owned attributes cannot be supplied, changed, or removed by callers:

- `application_hash`: SHA-256 content identity of submitted workflow source,
  opaque params and limits. Params remain uninterpreted; changing any of them
  changes identity without privileging keys such as `model` or `prompt`.
- `sim_model`, `sim_thinking`: the scenario-owned simulator's resolved model and
  effort keyword (`provider_default` when omitted; explicit `none` is distinct).
  There is no single agent-model attribute because one program may invoke many
  models; callers can record readable `model`/`variant` attributes explicitly.
- `scenario_id`, `scenario_revision`, `scenario_hash`: the pinned world identity.
- `simulation_backend`, `step_budget`, `token_budget`: effective execution
  configuration.
- `workspace_hash`: SHA-256 of sorted extracted paths and file bytes, independent
  of zip ordering, compression, timestamps, and archive name. No workspace
  corresponds to the empty-content hash. This fingerprints the uploaded seed,
  not later simulated writes.

`label` is editable but special: the UI displays it as the investigation's
name. Other custom attributes have no implicit meaning. A label edit does not change
the default grouping identity. If the caller explicitly groups by `label`, it
becomes an identity key like any other selected attribute; editing it then regroups.

```json
{"attributes":{"label":"Warm variant","campaign":"support"}}
```

Supply editable attributes on creation, or PATCH them later. PATCH accepts `grades`
and/or `attributes`, each merged by key; `null` deletes an editable entry. Validation
happens before either map changes, so a rejected read-only attribute change cannot
partially apply accompanying grades. Responses echo both full maps.

## Axes and grades

Graded axes are caller-PATCHed finite numbers. Names use the same allow-pattern
as attribute keys. Arbitrary scales are supported; no grade is interpreted against
traces. Measured axes cannot be PATCHed:

| Axis | Better | Source |
|---|---|---|
| `put_input_tokens`, `sim_input_tokens` | lower | Run totals by role |
| `put_output_tokens`, `sim_output_tokens` | lower | Run totals by role |
| `put_cache_read_tokens`, `sim_cache_read_tokens` | higher | Run totals by role |
| `put_cost_usd`, `sim_cost_usd` | lower | Catalog-estimated cost, absent if unpriced |
| `steps_per_trace_avg`, `_min`, `_max`, `_stdev` | lower | Completed traces; population stdev |

A step remains one tool call or one final completion. Direction is supplied
at frontier request time, not stored with a grade. Reserved directions cannot
be contradicted.

## Request

```json
{
  "group_by": ["application_hash", "scenario_id", "scenario_revision"],
  "axes": [
    {"name":"put_cost_usd", "better":"lower"},
    {"name":"quality", "better":"higher"}
  ]
}
```

`POST /api/frontier?format=json` supports one or more axes. `format=svg`
requires exactly two. Omitted `group_by` defaults to the example above: repeats
average only when both the application and pinned scenario revision match.
`group_by: []` makes one group. Missing attributes form explicit null-valued
groups in the API, not silent exclusions; null is distinct from the string
`"null"` or `""`. Human labels render absent system facts as `—` and absent
caller attributes as `(unset)` rather than presenting the JSON token `null` as a
value.

Malformed/unknown request fields are rejected. Invalid axes, duplicate grouping
keys, and direction conflicts yield typed validation problems. In particular,
the former `investigations` selection field is rejected, not ignored. The empty
job store is valid and produces an empty plot.

## Response and backlog

Each point exposes:

- `id`: stable group identity derived only from grouping attribute names/values.
- `attributes`: grouping values, with missing values represented as JSON null.
- `label`, `color`: presentation, not identity. The label is the slash-separated
  combination of selected attribute values in `group_by` order (for example
  `gpt-5.6-luna/low/prompt-a1b2c3d4`), not an opaque group hash. Caller-owned
  values are shown completely; model names use their basename and hashes use a
  labeled prefix. Full source values remain in `attributes`; presentation
  collisions receive a stable suffix.
- `investigations`: all member ids, including unfinished/excluded members.
- `included`: exactly the cohort used for every coordinate.
- `excluded`: one entry per non-contributor, naming its investigation, status,
  `missing_grades`, and `missing_axes` (unavailable measured values).
- `values`: means per requested axis, or null when there are no contributors.
- `on_frontier`: dominance result, or null when pending.
- `dominated_by`: group ids, not investigation ids or labels.
- `preliminary`: true while any group member is excluded.

Exclusion status distinguishes `running`, `failed`, `awaiting_grades`, and
`unavailable`. A worker that finished without any completed traces (all-error
or zero-scenario no-op) is excluded as `failed`, even if its job lifecycle says
`done`. Partial runs with traces can contribute; judging their adequacy remains
the caller's job. Missing grade names are surfaced even for running/failed members.
A completed investigation lacking either requested coordinate contributes to
neither mean. Never average X over one cohort and Y over another.

Missing data is **not** a whole-request error. An agent reads `excluded`, GETs
the named investigations' traces, PATCHes missing grades, and refreshes the
same frontier request. Failed jobs and unpriced costs remain visible; a group
can remain preliminary until such members are removed or become usable.

## Dynamic behavior and drawing

The frontier is recomputed from a consistent snapshot of all jobs on every
POST. The UI polls investigation data and refreshes after arrivals, completion,
external grade/attribute PATCHes, and deletion. No new subscription protocol is needed.
Late HTTP responses must not replace a newer selection/plot; polling must not
resurrect a deleted investigation. Turning auto-refresh off pauses polling.

All groups with coordinates participate in dominance, including preliminary
ones. A dominates B iff it is no worse on every requested axis and strictly
better on at least one. Identical means do not dominate each other. A group's
position **and frontier membership** can change as evidence arrives.

SVG uses a scatter and the axis-aligned boundary of the observed dominated
region. Lower-is-better axes are reversed so up-and-right is always better.
The UI's transpose control swaps the first two selected metrics while each
metric retains its own better-direction. The staircase turns vertically at each
current frontier point before continuing
to the next and stops at the rightmost observation; it never claims an
interpolated or unobserved configuration. A permanent low-opacity fill lightly
shades the union of rectangles dominated by observed frontier points; it has no
hover animation, special cursor, or tooltip. Filled circles denote frontier points;
hollow circles denote dominated points. Preliminary markers add reduced
opacity and a dashed outer ring; this distinction is separate from dominance.
Groups with no contributors are listed as pending without fake coordinates.
Point-adjacent labels are deliberately omitted to avoid clutter. A legend sits
to the right: plotted groups are ordered by projection onto the first principal
component of normalized rendered coordinates (after direction inversion), with
a deterministic left-to-right/top-to-bottom sign and x/y fallback for
isotropic or degenerate clouds. Pending groups follow by label. Entries fill
top-to-bottom, adding columns rather than shrinking the plot. Column width is
derived from the complete longest label (including conservative Unicode width),
so labels are never truncated or clipped; the container scrolls horizontally
when necessary. Each marker and
legend entry shares one focusable SVG group, so hover/focus highlights both and
dims unrelated groups while retaining frontier/dominated/preliminary shapes.
The UI shows member counts and the missing-grade backlog beside the plot.
Inline SVG surfaces, grid, text, and point outlines inherit the UI's light/dark
theme via semantic CSS variables, so toggling needs no refetch. Point hues,
dominance shapes, and preliminary markers stay unchanged. Standalone SVGs
have explicit Solarized Light fallback colors.

## Architecture and durability

`core/src/frontier/` owns attribute rules, grouping, means, dominance, and rendering.
The HTTP layer holds jobs, assembles snapshots, and routes requests; the UI
renders the response. Legacy core single-investigation helpers can remain for
standalone callers, but the HTTP/UI contract is grouped.

No persistence is added. Restarting loses jobs, attributes, and grades. Group ids are
stable for unchanged grouping values, but are not durable stored records.
No in-harness verdict, grade oracle, automatic grading, or comparability judge
is introduced.

## Validation and dogfood findings

- Deterministic core/HTTP regressions cover common-cohort means, stable ids and
  colors, null grouping values, missing grades, running/failed/pending members,
  deletion, immutable attributes, atomic PATCHes, canonical workspace hashes,
  all-error/zero-trace exclusions, extreme finite numbers, and SVG escaping.
- A live two-workspace run used the same PUT with different cosmetic ids. Both
  traces returned the requested `DONE`. The workspace hashes differed, prompt
  hashes matched, and both runs formed one point. Test grades 0.2 and 0.8
  produced mean 0.5 only after both were supplied. Renaming kept the group id;
  deleting one member recomputed the mean to 0.2. These grades were test
  coordinates, not a claim about model quality.
- Full-spec before/after caller probes ran through prompt-explore with
  Luna/low, the same operational question, and the manual verbatim in the user
  message. Before: grouping, persistent labels, and a grouped backlog were
  correctly reported unsupported. Initial after: the caller found those
  affordances but interpreted `put_/sim_cost_usd` shorthand as a literal axis.
  Explicit concrete axis names fixed the request construction. A separate
  stumble on nested failures prompted an explicit `result.result.failures`
  description; the final probe used that path. These probes inspected generated
  commands, not executed shell sessions; the live API exercise above separately
  checked actual grouping/PATCH/delete behavior.
- Browser checks cover both themes, 375px layout, real Rust-rendered SVG,
  preliminary markers, incoming/completed jobs, external grading, regrouping,
  deletion, pending/empty state, auth changes, and in-flight GET/PATCH races.
  Polls are serialized; dirty edits survive refreshes and save only their changed
  keys, retaining unrelated concurrent metadata edits.
- The terminology rename used identical full-spec direct and in-harness probes.
  Before, both callers consistently described the key/value map and exact JSON
  field as `tags`. After, both used `attributes` for create, PATCH, views, and
  grouped-point values; named the immutable provenance attributes; used exact
  `put_cost_usd`; and explicitly rejected `tags` as an invented alias. A live
  contract check separately verified legacy create/PATCH bodies return 400 while
  list/view/frontier responses expose only `attributes`.
- A same-spec label probe initially identified `point.label` correctly but said
  its formatting was unspecified. After the label contract was documented, it
  selected `point.label`, described the slash-separated attribute-value form,
  kept full values in `attributes` and stable identity in `id`, and
  explicitly rejected deriving a visible `g-<hash>` label. The live 8080 SVG
  and table separately displayed the expected value-derived labels.
- Legend rendering was checked through Chromium's DevTools Protocol. A 38-point
  synthetic plot filled three columns top-to-bottom without shrinking the plot;
  a real low/high eight-investigation run verified no text beside dots, the
  PCA-ordered right legend, bidirectional legend/dot hover, keyboard focus,
  unrelated-point dimming, accessible state/coordinates, and dark-theme colors.
  That live check caught transparent hollow-dot centers falling through to the
  panel; every marker now has an explicit pointer hit area. Headed inspection
  also caught label clipping; truncation and nested clips were removed, and
  live bounding-box checks now verify every full label stays inside the SVG.
