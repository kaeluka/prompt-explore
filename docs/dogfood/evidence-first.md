# Dogfood: evidence-first workflow and Lua capability boundaries

This records the before/after validation for the rework prompted by the
fresh-caller trial in `prompt-explore-experiments/2026-09-17-new-user/REPORT.md`.
Full artifacts (transcripts, source revisions, probes) live outside the repo at
`/tmp/pe-evidence-dogfood/` and
`/Users/kaeluka/code/prompt-explore-experiments/evidence-first-rework/`; the
report itself is the problem statement.

## What was investigated

The trial's caller recommended the hybrid Lua backend as a cheaper evaluation
path while assigning it a full grounding score, despite three observable
defects in the PUT-visible responses:

- `list_files(".")` returned `{"error":"invalid path"}` although the tool
  document said `.` is the repository root;
- an alternation/regex-shaped `search_text` pattern silently returned no
  matches, while literal searches then found the same content;
- root listings exposed the private `.prompt-explore` support directory inside
  a world declared to contain exactly nine files.

Because the caller's easy projection contained calls, workspace operations and
Lua outcome counts but not the actual `response` payloads, all three survived
into its conclusion. So did two workflow gaps: judgments stayed in local prose
until explicitly requested, and the first ZAI fanout burnt six jobs on a
catalog-listed but unfunded simulator.

## Changes exercised

- `GET /api/investigations/{id}/evidence`: one nonduplicated conversation,
  actual responses plus provenance, no terminal-progress duplication; present
  during failure with successful sibling exchanges retained.
- Deterministic execution evidence: `stop_reason`, step/token counters, frozen
  monotonic phase timings, a preserved unaccepted token-cutoff completion,
  expanded budget/ledger provenance attributes.
- Caller-owned `assessment` (summary/rubric/turn+exchange references) PATCHed
  atomically with grades/attributes.
- Reserved `.prompt-explore` private namespace; `.`/empty root aliases for
  directory listing; application-facing Lua capability view that cannot
  list/read/grep/write private artifacts.
- Authoring instructions that treat an unspecified search `pattern` as
  ambiguous and delegate rather than silently substituting literal grep.
- Spec-level worked loop: inspect responses → record assessment/grades →
  group explicitly → share URL; `generation_checked:false` on `/api/models`.

## Results

**Matched OpenAPI caller probes** (`evidence-before` → `evidence-after`, same
tasks, models, archives and budgets; before jobs `22f54da6`/`6db55208`, after
`9f29e7c8`/`15ee8d68`):

- Before, the configuration caller projected calls + `workspace_ops` +
  `lua_execution` outcomes, omitted `response`, awarded optimistic Lua grades
  and recommended Lua.
- After, both callers fetched `/evidence`, projected `call` **and** `response`
  per turn, and then: the configuration caller PATCHed an assessment that
  declined Lua as default and explicitly called `computed` execution rather
  than fidelity, citing the invalid-root and false-empty-search exchanges;
  the budget caller recognised `token_budget`, 16,061/16,000 PUT tokens, 13/16
  steps and no accepted final completion, and recorded that limitation as an
  assessment instead of a verdict.
- Caveat: the after world is simulator-rendered, and two simulated-shell
  confounds (a rejected invented `timeout` argument and one oversized
  projection replaced by prose) cost the review caller steps. The behavioral
  shift is real, but this is a single pair, not a rate.

**Matched Lua scenarios** (same nine-file queued-service worlds as the trial):

| run | root `list_files(".")` | ambiguous alternation search | outcome |
|---|---|---|---|
| trial (v0.5.0) | `invalid path` | silently `matches: []` as `computed` | no final answer |
| rework intermediate | nine files, no `.prompt-explore` | still silently `[]` as `computed` | correct classification, 1 fallback |
| rework final | nine files, no `.prompt-explore` | `fallback` → faithful multi-match response | correct classification, final answers |

Both final runs (`c9c3f6b2` developer, `0b965104` control) ended
`final_completion` with 9 `computed` + 1 `fallback` exchange, and the
vulnerable path was found while the all-safe control was cleared. The
intermediate row is the useful negative result: fixing the workspace boundary
alone left the false-empty search, which the strengthened authoring
instruction then delegated instead of faking.

## Independent validation round (`fixval`, 2026-09-18)

An independent agent re-ran a pre-registered protocol against this branch
(`/tmp/pe-fixval`, 16 jobs, $0.0329). It confirmed every named defect at the
level the report asked for — root aliases, private-namespace isolation, explicit
decline instead of false-empty search, genuine in-band errors still `computed`,
all four stop reasons, frozen equal execution records, server-only timings, and a
dashboard that leads with responses — and a held-out service (never seen by the
original caller) was classified correctly by both backends.

It also produced four findings that are addressed here, and one measurement.

**G1 — result envelope was still unstable across Lua regenerations.** Four
identical runs of one PUT/tool schema returned `list_files` as an object three
times and as a bare `entries` array once; the fallback search reply varied the
same way. Fix: the authoring instruction now requires an explicitly declared
shape to be rendered exactly and identically across runs, and an unspecified
shape to pass the capability result through unchanged rather than inventing or
unwrapping an envelope; the LLM simulator's system prompt carries the same
consistency rule. Matched re-check, four runs on the same requests:

| set | `list_files` envelope | ambiguous alternation search |
|---|---|---|
| before (4 runs) | object 3×, bare array 1× | one run added a literal handler → confidently empty `computed` |
| after (4 runs) | object 4× | explicit `fallback` 8/8 |

The middle column is the residual that made this worth a second pass: in the
first after-batch the model *revised* its program to add a literal search
handler, reasoning that the host capability is literal — reintroducing the
exact false-empty class the search instruction was written to prevent.

**G2 — delegation feedback invited that revision.** The harness told the
simulator that a declined Lua attempt "did not commit any workspace writes… you
may use workspace tools to repair/specialize the program for later calls",
which frames an intentional decline as a defect. The message now names the
decline reason, states that declining is a normal outcome, and forbids adding or
rewriting a handler for an underspecified tool; the authoring prompt says a
deferred handler must stay deferred across every revision.

**G2b — an unrenderable tool request had no name.** When a run dies inside a
tool batch, `turns` keeps the successful siblings but nothing identified the
request whose response does not exist. `execution.unrendered_call` now records
it (name + arguments), the failure text includes the tool name and a bounded
argument summary, and the dashboard/evidence show it.

**G4/G7 — field discoverability.** `JobView` now documents that execution
evidence lives at `progress.execution` (and `result.trace.execution` when done)
with no top-level alias, and points at
`progress.execution.budget_cutoff_completion` (`model_output`/`tool_calls`, not
`content`) and `progress.execution.unrendered_call`.

**G6 — default grouping deliberately keeps merging the backend.** Recorded as a
design decision in `docs/design/evidence-first.md`: the harness must not infer
which axis a caller is varying, so `simulation_backend` is selectable provenance
rather than a changed default.

**G3 — Lua setup is a large fixed cost.** The new phase timing shows
`preparing_tools_ms` of 24–54 s per run, often comparable to the whole PUT loop;
the validator's honest Lua pair measured ~24% total cost reduction at n=2, not
the baseline's claimed 89–90% simulator saving. No claim of a latency win is
made here.

**G1b — a DECLARED result shape was ignored (found by the uncoached caller, then
fixed).** The first G1 wording only said that an unspecified shape must be
passed through unchanged. An independent fresh caller that declared an explicit
`read` contract (`{path,total_lines,truncated,lines:[{n,text}]}`) in both the
description and `example_responses` received the harness workspace object
(`{content,start_line,end_line,...}`) verbatim instead, and had to count lines
by hand — a fidelity defect that also produced its prompt's citation slips. The
instruction now states that the declared shape WINS over what a lookup returns:
reshape to match it exactly, and forward the capability result unchanged only
when nothing is declared. Matched re-check on that caller's own request bodies
(same PUT prompts, tools, worlds, budgets; cheap simulator):

| set | `read` response | nested `lines[*]` |
|---|---|---|
| before (caller's runs, previous build) | `{content,start_line,end_line,path,total_lines}` | n/a (raw text) |
| after (3 completed runs, 19 exchanges) | `{lines,path,total_lines,truncated}` | `{n,text}` |

`list_dir.entries[*]` came back as the declared `{name,size,type}` (not the
workspace's `{name,kind}`) and `grep.matches[*]` as `{path,line,text}`, so
nested shapes are honored too. Residual: `list_dir` sometimes carried an extra
`truncated:false` the declaration did not mention — a harmless superset, but not
perfectly stable, and the reason this remains an instruction rather than a
guarantee. Classification was unaffected: the vulnerable service was flagged
and the safe one cleared in every completed run.

**Arm A — an uncoached fresh caller on the fixed build.** A separate agent, given
only "which of these two prompts should I ship, with evidence", working in a new
directory with no reminders about grading, evidence reading or handoff:

- read `/evidence` and the **actual** responses, and re-checked its cited line
  numbers against the simulator's returned bytes;
- **caught the simulator's contract deviation unprompted** (the G1b finding
  above) and told its principal to fix the tool schema before the next campaign;
- chose a decision axis from the measurements (steps used against the budget,
  cost per verdict, citation accuracy) and recommended the cheaper prompt,
  noting the more thorough one was one file away from `step_budget`;
- **PATCHed numeric grades and a populated `assessment` with an explicit rubric
  and turn-level evidence references, with no prompting to do so** — the D9
  workflow defect that the original trial needed an explicit "put it in the
  product" request to work around.

Two process notes for honesty: the caller's own probe jobs were lost when this
operator restarted the server mid-flight (a mistake on the operator side, not a
product defect — it recovered from locally saved evidence and re-ran what it
could), and one of our verification jobs stalled in `put_loop` with zero turns
for 15+ minutes while its provider call hung. That stall is reported as an
observation: nothing in the API distinguishes "provider call is wedged" from
"thinking hard", and only the frozen `put_tokens_used` next to a growing
`elapsed_ms` hints at it.

## What this does and does not show

- It does **not** validate generated Lua against a narrative, and adds no
  judge, consistency checker or narrative DSL. `computed` still means only
  "code ran"; the caller reads the response.
- It does not prove a latency or cost win. Timings are now server-measured
  monotonic values (`elapsed_ms`, phase split) rather than file mtimes, but the
  sample is tiny and noisy: final developer 70.8s vs. control 322.8s (the
  intermediate-pass control pair was 73.9s/111.0s). Read them as measurements,
  not as evidence that a backend is faster.
- Root-alias normalization and private-namespace isolation are deterministic
  plumbing. The search-contract behavior is a model instruction and remains
  best-effort: the caller still owns fidelity judgment.

## Verification

- `cargo test --locked`: 133 core unit + integration groups, 31 server tests,
  all passing, including the new `unrendered_call` retention in the runner and
  the evidence endpoint.
- `PLAYWRIGHT_MODULE=... node scripts/test-evidence-ui.cjs`: URL/share state,
  card-only filters, draft survival, assessment replace/clear, capped-run
  wording, `executed, fidelity unverified`, authenticated evidence download,
  XSS and source comparison.
- Live runs as tabulated above, with complete transcripts retained.
