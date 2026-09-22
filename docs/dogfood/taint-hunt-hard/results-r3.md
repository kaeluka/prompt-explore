# Round 3 — de-confounded contract, model sweep, thinking, decomposition

> Historical API note: these runs were captured before the workflow-only API
> cleanup. Raw evidence therefore contains the removed `put_model: "workflow"`,
> `prompt_hash`, and `workflow_hash` fields. Current runs expose one
> `application_hash` and put models/settings only on `workflow.invocations[]`.

**Question:** on the round-2 fixture, is the failure "fixture too hard" or
"nano too weak"? And what is the *cheapest* configuration that gets it right?

Fixture, workspace and `ground-truth.md` are **byte-identical to round 2**
(`workspace_hash=9aba312545d220880946a68f2c7a0666e7a19e2da9c8ec3b89ef5f28ca23d216`).
Only the tool contract and entry-point framing changed. Scenario:
`scn-aeca952b99bf` (`definition_hash=34ebd451d57986247dbdd70d82cd1093c3a52fd8bb5a3d650ef6fa228d2f32b2`).

## Phase 1 — the tool-contract gotchas were the experimenter's, and are fixed

`register_fixed.py` changed only:

- **`list_dir`** is now "STEP 1 — call this first", states the root is the only
  directory, and says to open each file by the exact returned name with no
  directory prefix. (Kills the phantom `app/` dir.)
- **`grep`** now spells out literal matching with a concrete counterexample:
  `execute|query` searches for that exact 13-character text and does **not**
  match `execute`. (The host `ctx.workspace.grep` has no regex mode, so a regex
  mode was not cheap to add; the contract is now unambiguous instead.)
- The opening message says to call `list_dir` first and read the exact names.

**Result (`fix` run, `gpt-4.1-nano`, one run):** nano now calls `list_dir("")`
first and reads all three files. The tool noise is gone. It still scored
**0/2 real flows** and its final answer *confirmed decoys* D2 (`top_rows`, int
cast) and D3 (`ordered_users`, whitelisted), with self-contradictory line
numbers (`db.py:7/11/15/19` for sinks that are at `db.py:9/15/26/32`). So the
sweep below measures taint reasoning, not tool guessing.

## Phase 2 — model sweep (single-agent baseline, temp 0, max_tokens 3000)

16 models, one run each. "rec" = real flows found (strict file:line); "FP" =
decoys claimed.

| model | rec | FP | steps | tokens | cost | note |
|---|:-:|:-:|---:|---:|---:|---|
| openai/gpt-5-mini | **1.00** | **0** | 5 | 7289 | $0.00509 | **only clean pass** |
| deepseek/deepseek-v3.2 | 1.00 | 1 | 9 | 22358 | $0.00488 | confirmed D2 |
| qwen/qwen3-235b-a22b-2507 | 1.00 | 2 | 8 | 12446 | **$0.00044** | confirmed D2+D3 |
| z-ai/glm-4.6 | 0.00 | 0 | 5 | 4831 | $0.00394 | **correct reasoning, wrong lines** |
| anthropic/claude-haiku-4.5 | 0.00 | 0 | 5 | 6595 | $0.01113 | wrong lines |
| openai/gpt-4.1-mini | 0.00 | 0 | 5 | 2984 | $0.00132 | wrong lines |
| openai/gpt-5-nano (default) | 0.00 | 0 | 6 | 9654 | $0.00161 | empty final answer |
| openai/gpt-5.4-nano | 0.00 | 1 | 9 | 5094 | $0.00098 | mixed F1, confirmed D3 |
| meta-llama/llama-3.3-70b-instruct | 0.00 | 1 | 5 | 8396 | $0.00088 | confirmed D2, wrong lines |
| qwen/qwen3-30b-a3b-instruct-2507 | 0.00 | 2 | 7 | 10303 | $0.00052 | wrong lines, D2+D3 |
| google/gemini-2.5-flash | 0.00 | 1 | 5 | 2767 | $0.00099 | wrong lines, D2 |
| amazon/nova-lite-v1 | 0.00 | 0 | 11 | 10874 | $0.00072 | response blocked by content filter |
| openai/gpt-4.1-nano (fixed contract) | 0.00 | 0 | 6 | 3382 | $0.00047 | confirmed D2+D3 in prose |
| google/gemini-3.5-flash-lite | 0.00 | 0 | 5 | 2912 | $0.00105 | wrong lines |
| mistralai/mistral-small-3.2-24b | 0.00 | 0 | 24 | 11162 | $0.00111 | empty final answer |
| google/gemini-2.5-flash-lite | 0.00 | 0 | 238 | 1507 | $0.00021 | one atomic batch of 234 greps, step_budget |

Two things worth separating from "failed":

- **glm-4.6 has the reasoning right and the citations wrong.** It traced all five
  routes, correctly marked D1/D2/D3 SAFE with the right mechanisms
  (`?` binding, `int()`, whitelist), and flagged only F1 and F2 — but cited
  `app.py:10`/`db.py:8` instead of `app.py:12`/`db.py:9`. Under a strict
  `file:line` rubric that is 0/2; the taint judgment itself was correct.
- The canonical sweep used `max_tokens=3000`. That is the wrong budget for
  reasoning models: gpt-5-nano and mistral returned **empty** finals, and
  gemini-2.5-flash-lite spammed a 234-call batch. Phase 3 fixes the budget.

## Phase 3 — thinking levels: the token budget is the lever, not effort

### gpt-5-nano (the cheapest model in play)

| thinking | max_tokens | runs | clean (rec 1.0, FP 0) | mean rec | mean FP | mean cost |
|---|---:|---:|:-:|:-:|:-:|---:|
| minimal | 12000 | 1 | 0/1 | 0.00 | 0 | $0.00035 |
| low | 3000 | 1 | 0/1 | 1.00 | 2 | $0.00092 |
| low | 12000 | 5 | **2/5** | 0.80 | 0.4 | **$0.00100** |
| default | 3000 | 1 | 0/1 (empty) | 0.00 | 0 | $0.00161 |
| default | 12000 | 3 | **3/3** | 1.00 | 0 | **$0.00233** |
| medium | 3000 | 1 | 0/1 (empty) | 0.00 | 0 | $0.00161 |
| medium | 12000 | 4 | **4/4** | 1.00 | 0 | **$0.00224** |
| high | 3000 | 1 | 0/1 (empty) | 0.00 | 0 | $0.00160 |
| high | 12000 | 1 | 1/1 | 1.00 | 0 | $0.00450 |

### gpt-5-mini (the canonical-sweep winner)

| thinking | max_tokens | runs | clean | mean cost |
|---|---:|---:|:-:|---:|
| default | 3000 | 3 | 3/3 | $0.00528 |
| low | 12000 | 3 | 3/3 | $0.00397 |
| high | 3000 | 1 | 0/1 (empty) | $0.00730 |
| high | 12000 | 1 | 1/1 | **$0.01148** |

**Finding:** spending more thinking does **not** buy precision here. Once the
output budget is large enough to finish, low/default/medium all reject the
decoys; high effort is strictly more expensive (nano: 4.5× low; mini: 2.9× low)
for no accuracy gain, and at a 3000-token budget high effort produces *no final
answer at all* (reasoning consumes the budget). The real lever is
**max_tokens**, which is what let the reasoning models complete their trace and
then reject D2/D3.

## Phase 4 — decomposition vs the solo winner

Two-stage enumerate→verify, run against the cheapest solo winner (gpt-5-nano
medium) and two others:

| configuration | rec | FP | tokens | cost | vs solo |
|---|:-:|:-:|---:|---:|---|
| gpt-5-nano medium, solo | 1.00 | 0 | 10781 | $0.00247 | — |
| gpt-5-nano medium, decompose | 1.00 | 0 | 17140 | $0.00308 | **1.25× cost, same result** |
| gpt-5-mini default, solo | 1.00 | 0 | 7289 | $0.00509 | — |
| gpt-5-mini default, decompose | 1.00 | 0 | 15099 | $0.01359 | **2.7× cost, same result** |
| deepseek-v3.2 high, solo | 1.00 | 0 | 16176 | $0.00360 | — |
| deepseek-v3.2 high, decompose | **0.00** | 0 | 43120 | $0.00967 | **worse and more expensive** |

**Drop it.** The decomposition never beats solo and once actively hurt — the
same pattern as round 2, now with a model that can actually do the task solo.

## Verdict — cheapest configuration that gets it right

**gpt-5-nano, single-agent baseline, `thinking` unset (provider default) or
`medium`, `max_tokens ≥ 12000`, temperature 0: ~$0.0023/run, 7/7 clean
(3/3 default + 4/4 medium), 0 false positives.**

- Absolute cheapest that *sometimes* works: `gpt-5-nano` + `thinking=low` +
  `max_tokens=12000` at **~$0.00100/run**, but only **2/5** runs were clean
  (two confirmed D2, one missed everything) — not reliable enough to recommend.
- `gpt-5-mini` is the cheapest model that passes even at a 3000-token budget
  ($0.0053/run, 3/3 clean), but it is ~2.3× the cost of the nano configuration.
- Everything cheaper than the nano configuration fails: qwen3-235b ($0.00044)
  and qwen3-30b both confirm decoys; gpt-5.4-nano / llama / gemini all confirm
  or miss; gpt-4.1-nano and gpt-4.1-mini fail outright.
- **Thinking effort is not the thing to spend on.** Token headroom is.
- **Decomposition is not the thing to spend on.** It is pure overhead here.

Round-3 totals: 50 investigations, 516,327 PUT tokens, **$0.1615**.

## Caveats

- Single fixture, few runs per cell; the low/medium distinction rests on 5 and 4
  runs. The all-zero cheap results are firm; the exact cheapest-clean boundary
  is directionally firm, not a tight confidence interval.
- Strict `file:line` scoring: glm-4.6 and claude-haiku-4.5 identified the flows
  but cited wrong line numbers and scored 0. If the deliverable tolerated
  "correct flow, approximate line," glm-4.6 (~$0.0039) would be a much cheaper
  pass. The task asks for exact `file:line`, so it is not scored as one.
- Two reasoning models returned empty finals at `max_tokens=3000`; that is a
  budget artifact, corrected in phase 3, not a capability verdict for those
  rows.
- `temperature=0` was requested for all runs; some reasoning endpoints ignore it.

## Reproduce

```bash
# server on 127.0.0.1:8080 with OPEN_ROUTER_API_KEY
python3 docs/dogfood/taint-hunt-hard/register_fixed.py            # prints scenario id
python3 docs/dogfood/taint-hunt-hard/r3.py run TAG baseline open_router::openai/gpt-5-nano 1 - 12000
python3 docs/dogfood/taint-hunt-hard/r3.py wait
python3 docs/dogfood/taint-hunt-hard/r3.py score
```

Raw final answers and metrics: `runs_r3_scored.json`. Decisive traces:
`evidence-r3/`. Grades are PATCHed onto all 50 investigations by
`annotate_r3.py`.