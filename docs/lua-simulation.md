# Caller-authored Lua tool implementations

Optional, per-tool, and authored by the caller. The harness never generates,
repairs or rewrites the source. A tool without `lua_source` is rendered by the
simulator LLM exactly as before; a tool with one is tried in the sandbox first,
and only what the handler declines reaches the model.

This is an accelerator for mechanical behavior, not a correctness oracle. Code
that executed can still be wrong: `computed` means "this code ran", never "this
response is faithful to the world".

## Where it lives

`lua_source` is a field of a scenario's tool:

```json
{
  "scenario": {
    "world": "...",
    "tools": [
      {
        "name": "read_file",
        "description": "path '.' or empty means root; returns {content} or {error}",
        "parameters": {"type": "object"},
        "side_effect": "read",
        "lua_source": "return function(args, ctx) ... end"
      }
    ],
    "simulation": {"lua": {"max_instructions": 2000000}}
  }
}
```

It is simulator-private: it never appears in the contract the prompt under test
sees, and it may encode ground truth the PUT is meant to discover. Registration
and edits PARSE the source (reporting the tool name and the Lua error for a
syntax mistake) but never execute it.

There is no enable switch. Supplying `lua_source` is what makes Lua run; the
`simulation.lua` object carries only resource limits.

## Handler contract

The chunk is UTF-8 Lua 5.4 text that RETURNS a handler function. It receives
`(args, ctx)` and returns `{response=..., state_patch=...}`. `response` must be
present; use `json.null` for null. Ordinary `{}` is an empty object and
`json.array({})` an empty array. Write tools return an object `state_patch`
(empty allowed); read tools cannot return a nonempty patch. Patches use the
existing shallow-merge convention: `json.null` deletes a key.

```lua
return function(args, ctx)
  if type(args.path) ~= "string" or args.path:sub(1, 5) ~= "repo/" then
    PleaseSimulateException("render this input with the simulator")
  end
  local result = ctx.workspace.read({path = args.path})
  if result.error then
    return {response = {error = result.error}}
  end
  return {response = {content = result.content}}
end
```

`PleaseSimulateException(reason)` **raises** a host-recognized signal; it is not
a string comparison, and it is not an error: it delegates that one call to the
simulator LLM. A string containing "PleaseSimulateException" is an ordinary
error. An ordinary crash, an invalid result, or a resource limit is recorded as
an `error` attempt and also delegates. The requested tool's semantics come from
its declared description and the world: if the contract is ambiguous, defer
instead of guessing, because a confident empty result is worse than a delegation.

`ctx.workspace.read/write/list_dir/grep` take the same argument tables and return
the same result objects as the simulator's own workspace tools. This is NOT the
host filesystem. The application-facing view hides the reserved `.prompt-explore`
namespace (a root listing, a read, an unscoped grep or a write cannot reach into
it). `list_dir` treats omitted `path`, `""` and `"."` as the root; traversal and
absolute paths are invalid. Workspace `grep` is a literal Unicode-substring
search, and Lua patterns are not regexes. `ctx.world_state` is a COPY of the
current state: persist changes through `state_patch` or workspace operations.

All workspace mutations are staged. Only a valid computed result commits the
staged workspace and its patch; delegation, crashes and limits discard both
before the LLM runs, and the discarded operations are reported separately so they
are never mistaken for committed world mutations. The computed or rendered
response enters the same simulator conversation, so later calls (computed or
rendered) see the established history.

## Testing it

`POST /api/scenarios/{id}/simulations` runs caller-submitted calls through the
same engine an investigation uses — before any investigation is spent, and
without a local Lua toolchain. See `docs/design/scenarios.md`. Prefer testing

* a computed call that returns a value,
* a call your handler declines (`fallback` in the provenance),
* a call whose arguments violate the schema (an in-band error that never reaches
  the handler),
* a write followed by a read of what it wrote (state/workspace consistency),
* a call you expect to crash, to see the rollback and the delegation.

Per call the response reports `lua_execution.outcome`
(`computed`|`fallback`|`error`), the `tool`, the exact `source_hash` that ran,
the diagnostic, and `discarded_workspace_ops`.

## Evidence

* The scenario definition carries the source; each run reports
  `implementations` (tool, source, `source_hash`) on `progress` and on
  `result.trace`.
* Each `tool_exchanges[].lua_execution` names the tool, the source hash, the
  outcome, the diagnostic, and any discarded operations. Committed operations
  stay in the ordinary `workspace_ops`.
* There are no generated revisions to display: a fix is a new scenario revision
  (fork a pinned scenario), and the hash tells you which traces ran the old one.

## Limits

All controls are positive and overridable under `simulation.lua`:

| Control | Default | Hard maximum |
|---|---:|---:|
| `max_memory_bytes` (Lua allocator only) | 16 MiB | 64 MiB |
| `max_instructions` (VM instructions and JSON conversion work) | 1,000,000 | 10,000,000 |
| `max_host_calls` | 128 | 1,024 |
| `max_host_bytes` (cumulative capability args/results) | 8 MiB | 32 MiB |
| `max_source_bytes` | 256 KiB | 1 MiB |
| `max_duration_ms` | 2,000 | 10,000 |
| `max_value_depth` | 128 | 128 (host-stack ceiling) |
| `max_result_bytes` (converted values; shared response/patch budget) | 1 MiB | 4 MiB |

`simulation.workspace.max_output_bytes` separately bounds construction of every
workspace read/list/grep result (default 1 MiB, hard maximum 4 MiB), before a
huge single-line file or listing is copied. Controls above the hard maxima are
rejected rather than weakening containment.

JSON conversion rejects cycles, oversized/deep/expanding structures, non-finite
numbers, mixed object/array keys, and unsigned integers above Lua's exact
signed-integer range rather than silently rounding them. The VM has no host IO,
no `require`/FFI, no debug facilities or error-catching escapes, and no
randomness or clock access.

**This is in-process containment, not an OS isolation boundary.** CPU deadlines
are cooperative: a native Lua C operation or a synchronous host call cannot be
interrupted mid-call. Lua allocator limits are not whole-process memory limits;
workspace inputs and Rust bookkeeping have their own bounds. Do not present this
as a hardened multi-tenant code-execution service.

## What earlier live probes found (kept as evidence)

These runs predate caller-authored Lua; the mechanism (sandbox, delegation,
rollback, shared conversation) is unchanged, and the findings still explain the
failure modes worth probing for.

* **A weak generator produced confidently wrong answers.** An LLM-written
  handler used `string.match(sku, "^X([1-9]|1[0-9]|20)$")` — Lua patterns have no
  alternation — and returned wrong answers for all 20 SKUs while executing
  successfully, so runtime fallback never triggered. It cut simulator calls
  from 23 to 2 in one run, but the numbers were wrong: not a valid speedup.
  Caller-authored code does not remove this class of bug; it makes it a fixed,
  inspectable revision instead of a fresh lottery each run.
* **Delegation and continuity work.** A computed write followed by a delegated
  audit let the LLM read the updated state and report it, and mixed
  computed/rendered exchanges shared one conversation and one world state.
* **Setup does not always pay off on short traces.** Hybrid runs with a handful
  of tool calls sometimes cost more wall clock than LLM-only, because the
  implementation exists whether or not it is used. Test the probe before
  running a campaign.
* **Probe framing matters.** Handlers written against an ambiguous contract
  returned confidently wrong shapes; the fix was to sharpen the tool contract or
  the world, not to add harness enforcement.
