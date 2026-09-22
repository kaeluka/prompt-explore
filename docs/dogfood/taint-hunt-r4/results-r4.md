# Round 4 — does the winner generalize?

> Historical API note: these runs were captured before the workflow-only API
> cleanup. Raw evidence therefore contains the removed `put_model: "workflow"`,
> `prompt_hash`, and `workflow_hash` fields. Current runs expose one
> `application_hash` and put models/settings only on `workflow.invocations[]`.

Config under test: **`open_router::openai/gpt-5-nano`, single agent, temperature
0, thinking unset, `max_tokens=12000`**, read-only tools, language-general
taint-audit prompt. Prompt/workflow handoff: `HANDOFF.md`. Ground truth:
`ground-truth.md` + `keys.json`.

The fixtures are byte-pinned per scenario; each tool call is Lua-served exact
(`lua_computed_calls == calls`, 0 fallback).

## Core fixtures — winner config, 2 runs each

| fixture | language / sink | real flows | run | recall | false pos | said NONE | steps | tokens | cost |
|---|---|:-:|:-:|:-:|:-:|:-:|:-:|---:|---:|---:|
| exec-js | JS / child_process | 2 | 1 | 1.00 | 0 | – | 8 | 17779 | $0.00333 |
| exec-js | | | 2 | 1.00 | 0 | – | 5 | 9286 | $0.00183 |
| long-chain | Python / SQL | 2 | 1 | 1.00 | 0 | – | 7 | 11753 | $0.00200 |
| long-chain | | | 2 | 1.00 | 0 | – | 6 | 9629 | $0.00158 |
| safe-app | Python / SQL + shell | **0** | 1 | n/a | **0** | **yes** | 5 | 7257 | $0.00105 |
| safe-app | | | 2 | n/a | **0** | **yes** | 5 | 6737 | $0.00090 |
| noisy | Python / SQL + shell, 6 files | 2 | 1 | 1.00 | 0 | – | 8 | 15824 | $0.00270 |
| noisy | | | 2 | 1.00 | 0 | – | 8 | 14506 | $0.00217 |
| dynamic | Python / SQL via dict dispatch | 1 | 1 | 1.00 | 0 | – | 6 | 7920 | $0.00100 |
| dynamic | | | 2 | 1.00 | 0 | – | 5 | 8109 | $0.00158 |

**10/10 correct**: every real flow found at the exact `file:line`, **zero false
positives**, and the safe app answered `NONE` in both runs. The winner
generalizes across a second language (JS), a second sink family (command
execution), a 4-file rename chain, dict-based indirect dispatch, and a
genuinely-safe app. Mean cost **$0.0018/run** (range $0.0009–$0.0033).

## Degraded configs — same fixture, to confirm the budget cliff generalizes

| fixture | tag | thinking | max_tokens | recall | false pos | said NONE | tokens | cost |
|---|---|---|:-:|:-:|:-:|:-:|:-:|---:|---:|
| exec-js | degraded-3k | default | 3000 | 0.00 | 0 | – | 21987 | $0.00283 |
| exec-js | degraded-low | low | 12000 | 0.50 | 1 | – | 7260 | $0.00097 |
| safe-app | degraded-3k | default | 3000 | n/a | 0 | **no** (empty) | 9632 | $0.00163 |

The cliff is real and not a round-1 quirk:

- **3000 output tokens → empty final answer** on exec-js and on the safe app
  (reasoning consumes the completion budget). The safe app did not even reach
  `NONE`, so it fails the false-positive test by omission.
- **`thinking=low`** is cheaper but unreliable: it found the first flow's sink
  with the wrong source line (`server.js:9` vs `8`) — scored as a false positive
  — and got the second flow right. Half the job at half the price.

## Scale boundary — repo size

| fixture | files / lines | budget | run | recall | false pos | steps | tokens | cost | stop |
|---|---|:-:|:-:|:-:|:-:|:-:|---:|---:|:--|
| big | 26 / 399 | 20 / 40k | 1 | **0.00** | 0 | 18 | 42142 | $0.00102 | token_budget |
| big | | | 2 | **0.00** | 0 | 18 | 42049 | $0.00115 | token_budget |
| big | | 60 / 250k | 1 | 1.00 | 0 | 28 | 81359 | $0.00365 | final |
| big | | | 2 | 1.00 | 0 | 28 | 80954 | $0.00401 | final |
| huge | 66 / ~1000 | 80 / 500k | 1 | **0.50** | **1** | 80 | 418246 | $0.00913 | final |
| huge | | | 2 | **0.50** | **1** | 68 | 351354 | $0.00665 | final |

Two distinct boundaries:

1. **Budget, not capability, at ~26 files.** With the 40k default the agent read
   17 files serially, exhausted the whole-investigation token budget at step 18,
   and returned an **empty answer**. With 250k tokens it passed 2/2 at 28 steps.
   The fix is to scale the budget with file count.
2. **Accuracy degrades at ~66 files.** Even with 500k tokens the model still
   identified *both* flows but cited `runner.py:4` (the `def execute` line)
   instead of `runner.py:5` (the `subprocess.run` sink) in both runs. Under a
   strict `file:line` rubric that is 0.5 recall + 1 false positive. This is a
   line-citation failure at scale, not a lost flow — but the deliverable asks
   for exact lines, so it counts as a failure.

## Totals

Round 4: **19 investigations, 1,163,783 PUT tokens, $0.04918**. Simulator cost
$0.00 throughout.

## Verdict

The winner **generalizes** across the tested envelope: two languages, two sink
families, 4–6 file call chains including indirect dispatch, and a
correctly-identified no-flow app. It is reliable up to ~26 files / ~400 lines
*with a budget that scales with file count*, and it degrades to line-citation
errors around ~66 files / ~1000 lines. The decomposition was not re-tested here;
round 3 already showed it never beats solo.

Full plain-terms deliverable, failure modes and boundary: `HANDOFF.md`.

## Reproduce

```bash
python3 register_r4.py                 # registers all fixtures, prints ids
python3 r4.py run exec-js winner open_router::openai/gpt-5-nano 1 - 12000
python3 r4.py wait && python3 r4.py score
# larger repos: R4_MAX_STEPS=80 R4_MAX_TOKENS=500000 python3 r4.py run huge ...
```

Run rows + final answers: `runs_r4_scored.json`. Decisive traces: `evidence-r4/`.
Grades PATCHed by `annotate_r4.py`.