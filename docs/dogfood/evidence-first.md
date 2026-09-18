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
  all passing.
- `PLAYWRIGHT_MODULE=... node scripts/test-evidence-ui.cjs`: URL/share state,
  card-only filters, draft survival, assessment replace/clear, capped-run
  wording, `executed, fidelity unverified`, authenticated evidence download,
  XSS and source comparison.
- Live runs as tabulated above, with complete transcripts retained.
