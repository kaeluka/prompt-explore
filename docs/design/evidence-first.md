# Evidence-first investigations

A fresh-caller taint-review trial exposed a workflow failure, not a need for an
in-harness judge: a caller treated successful Lua execution and workspace reads
as proof of faithful simulation. It recommended the cheap backend despite
invalid root listings and false-empty searches. It also kept judgments in local
prose, confused token/step exhaustion, and timed jobs using response-file mtimes.

## Representation and execution facts

`GET /api/investigations/{id}/evidence` is the preferred reading/archival surface.
It contains one `turns` array with every actual tool response and all supporting
provenance, plus the world, PUT, resolved controls, original budget, input values,
Lua revisions, failure/usage and caller annotations. Unlike the polling view it
does not repeat terminal turns in both progress and result. It is also available
while running and after failure, retaining successful sibling exchanges.

Neither workspace operations nor `lua_execution.outcome=computed` establish
fidelity. A handler can successfully return the wrong error or empty result.
Read the actual response against the tool contract and world. The endpoint never
summarizes or judges it.

Core `RunExecution` records:

- `stop_reason`: null while active; `final_completion`, `step_budget`,
  `token_budget`, or `runtime_failure` when stopped.
- `steps_used`: successful tool exchanges plus an accepted final completion.
  Sibling batches remain atomic and can cross the step limit.
- `put_tokens_used`: provider-reported PUT input + output, including repeated
  conversation history. Simulator usage is separate. A completion which crosses
  the token cap is retained as `execution.budget_cutoff_completion`: content,
  thinking and raw tool requests, explicitly unaccepted and not executed. No
  simulated responses are invented for those requests.
- `timing`: monotonic `elapsed_ms`, `resolving_inputs_ms`, `preparing_tools_ms`,
  `put_loop_ms`. Live snapshots refresh the active phase; terminal values freeze.

The polling view retains `budget`, epoch-millisecond `started_at`/`finished_at`,
and execution evidence in progress/trace. `done` means a trace was recorded,
not that the PUT produced a final answer or behaved well. Even an empty text-only
completion is a final completion; do not derive stopping conditions from text.
Durations are lower-is-better measured frontier axes. Capped traces remain
candidates; the caller owns their interpretation and comparability.

## Caller-owned assessment

PATCH accepts optional `assessment` alongside grades and attributes:

```json
{
  "grades": {"quality": 0.5},
  "assessment": {
    "summary": "The final conclusion looks right, but the simulated root listing was wrong.",
    "rubric": "quality: 0..1, higher is better; this is one case, not corpus precision",
    "evidence": [{"turn": 0, "exchange": 0, "note": "Tool promised '.', response rejected it"}]
  }
}
```

That score is an example, not a prescribed grading algorithm. Prefer assessment
alone when a numeric score is unjustified. Maps merge; an assessment object
replaces the whole value, null clears, omission preserves. All supplied changes
validate atomically. Text is limited to 65,536 UTF-8 bytes in total and 256 existing
zero-based turn/exchange references. Bounds checks validate location, not truth.
No requirement to grade every informative trace, no automatic verdict, no
persistence beyond the existing in-memory job lifetime. Updating an assessment
leaves grades intact: clear or revise stale scores when the new interpretation
invalidates them; a warning alone does not remove a grade from the frontier.

## Comparisons and handoff

The dashboard supports exact-AND attribute filters for **cards only**. All jobs
remain frontier candidates. A share link encodes JSON `group_by`, `axes` and
`attributes` in the URL; it never includes authentication. Back/forward restores
view state. Draft assessments, grades and attributes survive regrouping/filtering.

System provenance adds `simulation_backend` (`llm`/`lua`), `step_budget`, and
`token_budget` (decimal or `unlimited`). Group by the actual variables being
compared. The existing default PUT-model/thinking/prompt grouping deliberately
does not infer a backend experiment; without adding the backend key it merges
backends. Missing keys still form explicit null groups.

Lua sources can be viewed side by side across investigations. Fresh runs generate
fresh programs: changing a budget and observing a different adapter is not
proof that the budget caused the behavioral difference.

## Lua workspace capability boundaries

Application Lua handlers accept `.` or empty directory path as root. Their
workspace view cannot list/read/grep/write `.prompt-explore` support artifacts.
Native simulator authoring tools can still edit the private program, and source,
revisions and setup operations remain evidence. Uploads colliding with the
reserved private namespace are rejected. This is capability isolation, not
validation of narrative semantics.

Host grep is literal substring search; Lua patterns are not regexes. Authoring
instructions require adapting the caller's contract or delegating unsupported
semantics, rather than returning false-empty results. An unspecified search
'pattern' is ambiguous: leave that handler to the LLM rather than arbitrarily
choosing the host's literal semantics. This remains a model instruction, not
semantic enforcement. Legitimate in-band tool
errors remain data and do not automatically trigger fallback. There is no new
cache, narrative DSL or semantic consistency checker.

## Model discovery

`GET /api/models` reports `generation_checked:false`. Catalog/configuration
access does not establish inference access or credit balance. No hidden charged
probe occurs. Try a small investigation with both chosen roles before fanout;
quota/funding failures call for provider/operator action, not prompt edits.

## Validation

Unit and integration tests cover snapshot/finalization, caps and failures,
assessment atomicity, nonduplicated evidence/auth, capability boundaries and
frontier timings. `scripts/test-evidence-ui.cjs` checks the UI with a mocked API.
Matched live Lua scenarios and spec-in-world caller probes are documented in
`docs/dogfood/evidence-first.md`; simulation reliability remains caller-owned.
