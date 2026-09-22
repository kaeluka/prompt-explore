# Handoff — the configuration to give someone

> Historical API note: these runs were captured before the workflow-only API
> cleanup. Raw evidence therefore contains the removed `put_model: "workflow"`,
> `prompt_hash`, and `workflow_hash` fields. Current runs expose one
> `application_hash` and put models/settings only on `workflow.invocations[]`.

**One-line version:** a single `ctx.run_agent` call on
`open_router::openai/gpt-5-nano`, temperature 0, no explicit thinking level,
`max_tokens=12000`, with a language-general taint-audit system prompt, against a
scenario whose tools are read-only (`list_dir` / `read_file` / `grep` served
exact). On repositories up to ~26 files / ~400 lines it finds every intended flow
with zero false positives; on ~66 files it starts mis-citing line numbers.

## The exact workflow

### `lua_source` (also in `baseline_r4.lua`)

```lua
return function(params, ctx)
  local a = ctx.run_agent {
    name = "baseline",
    prompt = params.system,
    model = params.model,
    input = ctx.input,
    tools = { "list_dir", "read_file", "grep" },
    controls = params.controls or { temperature = 0, max_tokens = 12000 },
  }
  return {
    answer = a.output,
    stop_reason = a.stop_reason,
    steps = a.steps_used,
    tokens = a.tokens_used,
    failure = a.failure,
  }
end
```

### `params`

```json
{
  "model": "open_router::openai/gpt-5-nano",
  "system": "<the prompt below>",
  "controls": { "temperature": 0, "max_tokens": 12000 }
}
```

### System prompt (verbatim)

```text
You are a static-analysis security auditor. Use the tools list_dir, read_file and grep to inspect the repository. Untrusted input enters through request query parameters, headers, and bodies. Dangerous sinks include SQL execution (execute/executescript, string-built SQL), command execution (child_process exec/execSync/execFile/spawn, subprocess with shell=True), eval, and file paths.

A path from a source to a sink is an exploitable flow ONLY if the untrusted value reaches the sink without an effective defense. Effective defenses include: parameter binding (a `?` placeholder with a bound parameter tuple), a type cast (e.g. int()), a whitelist/allowlist restricting the value to a fixed set, shell quoting (e.g. shlex.quote), and argument arrays passed to a no-shell API. Parameterized, cast, whitelisted, quoted and argument-array paths are NOT vulnerabilities. Verify every claim by reading the actual code.

Report ONLY confirmed exploitable flows, one per line, exactly:
FLOW source=<file>:<line> sink=<file>:<line>

Do not report safe paths. If there are no exploitable flows, print NONE.
```

### Investigation budget (scale it — this is the main footgun)

```json
{"max_steps_per_trace": 20, "max_tokens": 60000}
```

`max_steps_per_trace` is the **whole-investigation** step budget; `max_tokens`
is the **whole-investigation** token budget across every turn. Both must scale
with repository size, because the agent reads files one at a time:

| repo size | steps | tokens | observed |
|---|---:|---:|---|
| ≤10 files | 20 | 60k | ok (~10k tokens used) |
| ~26 files | 40 | 150k | needed 28 steps / 82k |
| ~66 files | 90 | 600k | needed 68–80 steps / 350–420k |

A rough rule: `steps ≥ 2 × n_files`, `tokens ≥ 8000 × n_files`.

### Scenario tool contract

Tools must be read-only and served exact (the round-3 fixed contract):

- `list_dir(path?)` — first call; empty path lists the root; files live there.
- `read_file(path)` — exact bytes.
- `grep(pattern, path?, case_insensitive?)` — **literal substring**, not regex.
  Say so explicitly in the description; models otherwise pass `a|b` and get
  nothing. There is no regex mode in `ctx.workspace.grep`.

The full scenario definitions are `fixtures/exec-js`, `fixtures/safe-app`,
`fixtures/long-chain`, `fixtures/noisy`, `fixtures/dynamic` (`register_r4.py`).

## What it costs

Per run, gpt-5-nano PUT tokens only, simulator cost $0.00 (Lua-served tools):

| repo | mean | range |
|---|---:|---:|
| ≤6 files (5 fixtures × 2 runs) | **$0.0018** | $0.0009 – $0.0033 |
| ~26 files (with 250k budget) | $0.0038 | $0.0037 – $0.0040 |
| ~66 files (with 500k budget) | $0.0079 | $0.0066 – $0.0091 |

Small-repo runs averaged ~11k tokens; the 26-file runs ~81k; the 66-file runs
~385k.

## Known failure modes (all observed)

1. **Whole-investigation token exhaustion on multi-file repos.** The agent reads
   files serially; input tokens accumulate every turn. At the default 40k budget
   the 26-file repo returned an **empty answer** (`stop=token_budget`) in both
   runs. Fix: raise `max_tokens` with repo size.
2. **`max_tokens` (per completion) too small = empty final answer.** Reasoning
   models can spend the entire completion budget on reasoning. At 3000 the
   winner returned empty on exec-js and safe-app; `thinking=high` at 3000 also
   returned empty. Use ≥12000.
3. **`thinking=low` is not reliable.** It is ~$0.0010/run but in 5 nano runs it
   was clean only 2/5, confirmed decoy D2 twice, and missed a flow once. On
   exec-js it found 1/2 flows plus a wrong-line false positive.
4. **Line-citation degradation at scale.** At 66 files the model still found
   both flows but cited `runner.py:4` (the `def` line) instead of `runner.py:5`
   (the sink). Under a strict `file:line` rubric that is a miss plus a false
   positive. It is a citation error, not a reasoning error.
5. **Weak/cheap models confirm decoys.** qwen3-235b ($0.0004) and qwen3-30b
   confirmed D2/D3; gpt-5.4-nano and llama confirmed one decoy each. This is why
   the config is gpt-5-nano at 12k tokens, not the absolute cheapest model.
6. **Format drift.** Some models answer in prose; the workflow depends on
   `FLOW source=... sink=...`. The prompt must demand it, and a scorer should
   tolerate the `FLOW <file>:<line> sink=...` variant.
7. **High thinking is pure cost.** gpt-5-nano high = 4.5× low for the same
   result; gpt-5-mini high = ~$0.0115 vs $0.0037 low. Do not enable it.

## The honest boundary

- **Reliable (2/2 exact, 0 false positives on both runs):** every fixture up to
  **~26 files / ~400 lines**, across Python and JavaScript, SQL and command
  execution, a 4-file rename chain, dict-based indirect dispatch, and a
  no-flow "safe app" that it correctly answered `NONE` twice.
- **Degrades at ~66 files / ~1000 lines:** both flows still identified, but one
  sink cited on the `def` line rather than the body; 350–420k tokens and 68–80
  steps. It is at the edge of its navigation ability at this size.
- **Not tested / unknown:** repositories much beyond ~1000 lines or a few
  hundred files; minified or generated code; heavy metaprogramming (beyond the
  dict-dispatch case); true cross-service flows. Do not claim reliability there.
- **Cost is the first thing to break, not accuracy.** Scale the investigation
  budget with file count or the run returns nothing.

**Do not use the two-stage enumerate→verify decomposition.** It never beat solo
and once failed outright (round 3); it is not part of this handoff.