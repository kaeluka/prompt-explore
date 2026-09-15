# Experimental hybrid Lua simulation

Feature branch: `feature/lua-tool-simulation`. This is a prototype, not part of
the v0.4.1 release. No automatic cache and no in-harness semantic judge.

## Using it

Build this branch, start the server, and add this fragment to an ordinary
investigation request:

```json
{
  "conversation_controls": {
    "lua_simulation": {}
  }
}
```

An empty object enables bounded defaults. Omit/null keeps LLM-only simulation.
The job view echoes the actual limits in `conversation_controls.lua_simulation`;
check this rather than assuming an older server understood a new field.

Runnable requests are preserved in [lua-hybrid.json](examples/lua-hybrid.json)
(write/read/audit) and [lua-inventory.json](examples/lua-inventory.json) (21
lookups, including a negative case). Submit either to a configured server:

```sh
curl http://127.0.0.1:8095/api/investigations \
  -H 'Content-Type: application/json' \
  --data-binary @docs/examples/lua-hybrid.json
```

Remove `conversation_controls.lua_simulation` for the identical LLM-only
baseline. The examples keep the PUT on OpenRouter Luna/low but use OpenRouter
Terra for simulation with `sim_thinking_level` omitted (provider default). They
spend provider credits.

Input resolution happens first. If the PUT has tools, the harness creates a
valid fallback-only `.prompt-explore/tools.lua` in the per-trace in-memory
workspace (an existing uploaded file at that path is preserved). The simulator
gets a preparation turn and can specialize the module using its ordinary
workspace tools. It can also revise the source during later LLM fallbacks.
No tools means no preparation call. A fresh Lua VM loads the current revision
for every invocation; private globals/upvalues do not survive calls.

The simulator chooses how much to implement. Enabling the feature does not
promise fewer model calls: leaving every stub untouched is permitted, and
preparation then adds overhead. Narratives remain the ground truth. Source is
an unverified interpretation, not a compiled guarantee of the author's intent.

## Handler contract

A UTF-8 Lua 5.4 module returns a table keyed by exact PUT tool names. Each
handler receives `(args, ctx)` and returns `{response=..., state_patch=...}`.
`response` must be present; use `json.null` for null. Ordinary `{}` is an empty
object; `json.array({})` is an empty array. Write handlers return an object
`state_patch` (empty is allowed); read handlers cannot return nonempty patches.
Patches use the existing shallow-merge convention: `json.null` deletes a key.

Example of a partial implementation for a tool that reads repository files:

```lua
return {
  read_file = function(args, ctx)
    if type(args.path) ~= "string" or args.path:sub(1, 5) ~= "repo/" then
      PleaseSimulateException("render this input with the simulator")
    end
    local result = ctx.workspace.read({path = args.path})
    if result.error then
      return {response = {error = result.error}}
    end
    return {response = result.content}
  end
}
```

`PleaseSimulateException(reason)` **raises** a host-recognized signal; it is not
a string comparison. Missing handlers also delegate. Ordinary crashes, invalid
results, and resource-limit failures are distinct `error` attempts, followed by
LLM fallback. A string containing "PleaseSimulateException" is just an ordinary
error, not the delegation signal.

`ctx.workspace.read/write/list_dir/grep` take the same argument tables and
return the same result objects as the simulator's workspace tools. This is NOT
the host filesystem. The generated program is simulation machinery, not an
extra fact in the scenario's world; handlers must adapt workspace results to
the PUT tool's actual contract and inventory. `ctx.world_state` is a copy of
current state; use patches or workspace operations for persistent changes.

All Lua workspace mutations are staged. Only a valid computed response commits
the staged workspace and its patch. Delegation/crashes/limits discard it before
the LLM runs. Discarded operations are reported separately, never presented as
committed world mutations. The actual computed or LLM response enters the same
persistent simulator conversation, so subsequent fallbacks see previous calls,
responses, and current explicit world state without an LLM call for every
computed exchange.

## Evidence and UI

- `progress.scenarios[].simulation_program` is published during preparation,
  including on failures before the first PUT turn.
- `result.attempts[].simulation_program` preserves source revisions and setup
  workspace operations/reasoning alongside `resolved_inputs`.
- `tool_exchanges[].lua_execution` identifies the **zero-based revision** tried,
  `computed` / `fallback` / `error`, diagnostic, and discarded operations.
  Committed operations remain in ordinary `workspace_ops`.
- Each scenario's `phase` reports `resolving_inputs`, `preparing_tools`, or
  `put_loop`; concurrent scenarios can be in different phases.
- The UI displays escaped, copyable source and earlier revisions near resolved
  inputs, with neutral execution labels. Source is never browser markup.

## Limits and caveats

All controls are positive and overridable under `lua_simulation`:

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

`conversation_controls.workspace_max_output_bytes` separately bounds construction
of every workspace read/list/grep result (default 1 MiB, hard maximum 4 MiB).
This cap is applied before copying a huge single-line file or accumulating a
large listing, and the Lua bridge may impose a still-tighter remaining
host/result budget for an invocation.

JSON conversion rejects cycles, oversized/deep/expanding structures, non-finite
numbers, mixed object/array keys, and unsigned integers above Lua's exact
signed-integer range rather than silently rounding them. Request controls above
the hard maxima are rejected rather than weakening process containment. The VM cannot access
host IO, loading/FFI, debug facilities, error-catching escapes, randomness, or
a clock. Random/time capabilities are deliberately deferred.

**This is in-process containment, not an OS isolation boundary.** CPU deadlines
are cooperative: native Lua C operations and synchronous host operations cannot
be interrupted mid-call. Lua allocator limits are not whole-process memory
limits; workspace inputs and Rust bookkeeping have their own bounds. Do not
present this experimental backend as a hardened multi-tenant code-execution
service. No cross-platform replay guarantee is implied by Lua table iteration
order or by the stochastic LLM that generates code.

## What the live probes found (2026-09-15)

The current comparison keeps the PUT on OpenRouter Luna/low and uses OpenRouter
Terra for the simulator, with `sim_thinking_level` omitted so the provider chooses
its default. Hybrid and LLM-only pairs used identical worlds and PUTs, with
4096/8192 output-token limits. Counts include setup and JSON repairs; timings and
costs are **single concurrent samples**, not performance guarantees.
Deterministic tests separately cover input-dependent fallback within one handler,
history continuity, state updates, and rollback.

### Terra/default: correct long execution

For 21 separate lookups (`X1` through `X20` plus the deliberately absent `X01`),
four independently generated hybrid programs returned every result correctly.
Terra used an explicit loop once and valid Lua patterns/string checks in the
repeats; it did not repeat the invalid regex-style alternation from the earlier
Luna run. Every hybrid used 2 simulator completions and took about 12–20s, with
reported simulator cost about $0.0048–$0.0073. The one paired LLM-only run used
21 completions, about 34s, and $0.0270. This is encouraging repeated evidence
for this world, not a general semantic guarantee.

Terra also specialized both five-item lookup variants correctly, including the
missing SKU. Hybrid used 2 simulator completions versus 5. On these short traces,
setup did not pay for itself: one pair took about 16s versus 14s and cost about
$0.0106 versus $0.0062; the sequential pair took about 16s on both paths and
cost about $0.0040 versus $0.0028.

### Terra/default: mixed execution

A stock update was computed by Lua and a Lua read observed the updated count 11.
The `audit` handler deliberately raised `PleaseSimulateException`, and the LLM
fallback saw enough shared history/state to report that A had changed and was
currently 11. The PUT then accurately reported the complete set/read/audit
sequence. The fallback summary itself was less specific than the LLM-only
baseline: it omitted the intervening lookup. Hybrid and baseline each used 4
simulator completions; hybrid took about 24s versus 16s and cost about $0.0214
versus $0.0068. This demonstrates continuity, not a short-trace efficiency win.

### Earlier Luna/low counterexample retained

The first experiment used Luna with low simulator thinking. A 21-lookup hybrid
run dropped simulator completions from 23 to 2 and wall time from about 57s to
12s, **but its generated program returned wrong answers for all 20 existing
SKUs**. This was not a valid speedup result. The model wrote:

```lua
local digits = string.match(sku, "^X([1-9]|1[0-9]|20)$")
```

Lua patterns do not implement regex alternation. The implementation executed
successfully, so runtime fallback did not trigger. The evidence remains useful
as a failure of that weak setup and as a general reminder: stronger generation
repaired this observed case, but runtime fallback still handles crashes rather
than plausible-but-wrong computations. We did not silently repair either
program or add a deterministic semantic oracle.

### Probe framing mattered

An earlier corpus put Lua-specific delegation instructions in simulator notes.
The old LLM-only path sometimes rendered `PleaseSimulateException` as a tool
error instead of the required inventory response; one hybrid fallback also
omitted stock fields. Those were visible simulation failures, not wins. We
reran the corpus with those implementation-specific notes removed, keeping the
clean worlds identical across backends. The earlier evidence is retained.

Validation: 143 locked workspace tests and the release build passed. Browser
checks covered light/dark themes, live preparation with zero PUT turns, completed
programs/revisions, copy-to-clipboard, escaped source text, and explicit rollback
labels. Failure/HTML-looking-source browser fixtures are synthetic UI tests,
separate from the live model investigations above.

Full-spec caller probes before/after correctly moved from "unsupported" to the
exact opt-in field, program locations, revision provenance, rollback and shared
history. One probe caught schema minima of zero conflicting with documented
positive limits; the generated schema was corrected to match validation.

Local artifacts: `/tmp/pe-lua/` contains requests, full job views, timings, source
revisions in those job views, caller-probe answers, test logs, and UI screenshots.
Terra/default A/B and repeated runs are under `/tmp/pe-lua/terra/`. The earlier
Luna/low counterexample is `long-after.result.json`; its clean short runs are
`clean-after-v2-*.result.json`. There is no claim of direct-AWS verification.
