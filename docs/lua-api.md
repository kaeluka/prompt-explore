# Lua tool implementations: the handler API

This is the reference for `lua_source` on a scenario tool. It is served by the
running server at `GET /docs/lua`, so a caller that was pointed at the HTTP API
can read it without access to this repository.

A scenario tool carries an **optional** Lua implementation. Supplying source is
what makes Lua run — there is no enable switch. The harness never generates,
repairs, or rewrites supplied code; it parses the source when the scenario is
registered (a syntax error is a `400`, not a broken run) and executes it inside
the sandbox for every call of that tool.

## The handler

The chunk must **return** a function. It receives the tool-call arguments and a
context table, and returns one table:

```lua
return function(args, ctx)
  -- args: the tool call's arguments, decoded from JSON (so no nils in arrays,
  -- and JSON objects are Lua tables with string keys)
  -- ctx:  the run context (see below)

  return {
    response = { ... },        -- REQUIRED: the value the prompt under test receives
    state_patch = { ... },     -- optional: world-state updates (write tools only)
  }
end
```

* `response` is exactly what the prompt under test sees for that call. Shape it
  to match the tool's declared contract; do not leak extra keys.
* `state_patch` is applied only for a tool the scenario declared as a **write**
  tool. `null` values delete keys; it is a shallow merge into world state.
* A handler that cannot faithfully render a call should **decline** it:

  ```lua
  if not known[args.id] then
    PleaseSimulateException("only orders in the fixture are implemented")
  end
  ```

  Declining is a normal outcome, not a failure. The simulator LLM renders that
  one response from the world narrative, and the trace records
  `lua_execution.outcome = "fallback"`.
* A runtime error, a sandbox limit breach, or a **missing** implementation also
  delegates that one call to the simulator LLM. For a runtime error the outcome
  is `"error"`, and any workspace writes the handler staged are rolled back
  before the LLM runs. Missing implementations produce no `lua_execution` record
  at all.

Computed and LLM-rendered responses enter the **same** conversation, so a
delegated call keeps the established history.

## `ctx.workspace` — the run's private filesystem

`ctx` currently exposes one capability: `ctx.workspace`, with four operations.
They are the same operations the simulator LLM's own workspace tools use, so a
handler that forwards a result unchanged returns what the simulator would have
looked up. That is how a caller serves **exact file contents** (uploaded with
the scenario) instead of asking a model to reproduce them.

All paths are relative to the workspace root and use `/` separators. Every
operation returns a JSON-decoded table. Errors are values, never exceptions:
check for an `error` key.

### `ctx.workspace.list_dir({ path = "." })`

Lists the **direct** children of a directory (not recursive).

```lua
{ path = ".", entries = { { name = "src", kind = "dir" },
                          { name = "README.md", kind = "file" } },
  truncated = false }
```

`path` may be omitted, `""`, or `"."` for the root. Unknown directory:
`{ path = "...", error = "not found" }`. Invalid path: `{ error = "invalid path" }`.

### `ctx.workspace.read({ path = "src/main.rs" })`

Reads a file, at most `workspace_max_read_lines` lines (default 5000), from
`start_line` (1-based, optional) to `end_line` (optional).

```lua
{ path = "src/main.rs", content = "fn main() {}\n", start_line = 1,
  end_line = 1, total_lines = 1, truncated = false }
```

Missing file: `{ path = "...", error = "not found" }`.

### `ctx.workspace.grep({ pattern = "println" })`

Searches for a **literal** substring — not a regular expression. Optional `path`
restricts the search to a file or directory prefix; optional
`case_insensitive = true`.

```lua
{ pattern = "println",
  matches = { { path = "src/main.rs", line = 1, text = "fn main() { println!() }" } },
  truncated = false }
```

At most `workspace_max_grep_matches` matches (default 1000); results are also
byte-bounded.

### `ctx.workspace.write({ path = "notes.txt", content = "..." })`

Writes (creates or overwrites) a file in the run's private overlay. Visible to
later calls in the **same** run, never to another run (every run starts from the
same immutable seed), and never to the prompt under test except through the
handler's `response`.

```lua
{ path = "notes.txt", bytes = 12, ok = true }
```

The `.prompt-explore` path namespace is reserved and refuses application access.

## Resource limits

The sandbox is bounded by `simulation.lua` on the scenario (`LuaOptions`); every
field is an explicit override with a documented default and a hard ceiling. Zero
is invalid. Breaching any limit raises a runtime error, which delegates that one
call to the simulator LLM with `outcome = "error"`.

| field | default | meaning |
| -- | -- | -- |
| `max_source_bytes` | 262144 | largest accepted `lua_source` chunk |
| `max_instructions` | 1000000 | VM instructions per call |
| `max_duration_ms` | 2000 | cooperative wall-clock deadline per call |
| `max_memory_bytes` | 16777216 | Lua allocator limit for the call |
| `max_host_calls` | 128 | `ctx.workspace.*` calls per handler invocation |
| `max_host_bytes` | 8388608 | bytes the host may exchange per invocation |
| `max_result_bytes` | 1048576 | byte bound on a converted result |
| `max_value_depth` | 128 | JSON/Lua nesting depth (hard ceiling 128) |

The duration limit is **cooperative**: it is checked by the VM hook and around
Lua↔Rust conversion boundaries. A single long-running native call cannot be
preempted mid-operation, and the sandbox is in-process — it is not OS-level
isolation.

## What the caller should check afterwards

`computed` means the code ran; it does **not** mean the response was faithful.
Read the responses the prompt under test actually received, and read the
counters (`execution.lua_computed_calls`, `lua_fallback_calls`,
`lua_error_calls` on an investigation; the same three on a probe view) to see
whether your code served the run at all. A run whose handlers silently declined
or errored is model-rendered regardless of how good the source looks.

## Worked example: serve real files, delegate the rest

```lua
-- A `read_file` tool whose contract is
--   { path, content, truncated }  and  { path, error }
return function(args, ctx)
  if type(args.path) ~= "string" then
    return { response = { error = "missing required argument 'path'" } }
  end
  local read = ctx.workspace.read({ path = args.path })
  if read.error ~= nil then
    return { response = { path = args.path, error = read.error } }
  end
  return {
    response = {
      path = read.path,
      content = read.content,
      truncated = read.truncated,
    },
  }
end
```

```lua
-- A `lookup_order` tool that only implements the fixture's own orders.
local FIXTURE = {
  ["A-1"] = { status = "shipped", carrier = "DHL" },
}

return function(args, ctx)
  local order = FIXTURE[args.id]
  if order == nil then
    -- Let the simulator render anything outside the implemented subset.
    PleaseSimulateException("only fixture orders A-1 are implemented")
  end
  return { response = { id = args.id, status = order.status, carrier = order.carrier } }
end
```

## Testing before you spend an investigation

`POST /api/scenarios/{id}/simulations` runs caller-submitted tool calls through
the **same** engine an investigation uses — argument validation, sandbox limits,
delegation, workspace mutation, state patches — with no prompt under test and no
investigation created. Use it to develop and prove a handler before spending
runs. Each call reports its response and its provenance, so a fallback or an
error is visible immediately.