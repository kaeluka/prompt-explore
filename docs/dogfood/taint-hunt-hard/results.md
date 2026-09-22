# Round 2 — hard 3-file taint fixture: baseline vs. enumerate→verify

> Historical API note: these runs were captured before the workflow-only API
> cleanup. Raw evidence therefore contains the removed `put_model: "workflow"`,
> `prompt_hash`, and `workflow_hash` fields. Current runs expose one
> `application_hash` and put models/settings only on `workflow.invocations[]`.

**Model:** `open_router::openai/gpt-4.1-nano` for all agent stages (same as round 1).
**Fixture:** `docs/dogfood/taint-hunt-hard/workspace/` (app.py / handlers.py / db.py).
**Ground truth:** `ground-truth.md` — 2 real flows, 4 decoys, exact lines.
**Tool surface:** identical read-only `list_dir` / `read_file` / `grep` Lua handlers
copied from round 1; probe confirmed byte-exact serving (5/5 Lua-computed, 0 fallback).
**Scenario:** `scn-2b11bf9abd9a` rev 1, `definition_hash=76aedc0bacd0df24...`.

## Headline (honest)

**Nano cannot do this fixture.** Across all 8 runs it found **0 of 2** real
flows and, under the pinned FLOW-line rubric, claimed **0** decoys — because it
mostly never produced a FLOW line at all.

More importantly, the **primary** runs failed for a reason that has nothing to
do with taint reasoning: nano fumbled the tool surface. A **guided** arm that
fixed only that confusion was run as a control; nano still found 0 real flows,
and the one run that actually reasoned about the code **confirmed 2 decoys as
vulnerable**. So the failure is genuine, not an artifact.

The two-stage decomposition did **not** help. It produced no better recall, cost
more, and its verifier stage — the whole point of the design — is exactly where
the decoys were waved through.

## Primary protocol (as requested)

2 × single-agent baseline; 2 × enumerate→verify decomposition (both nano). Budget
10–18 steps / 12k–24k tokens, temperature 0.

| variant | run | real_flows_found | recall | false_positives | steps | put tokens | cost (USD) | stop |
|---|---:|:-:|:-:|:-:|---:|---:|---:|:--|
| baseline | 1 | 0/2 | 0.00 | 0 | 8 | 4354 | 0.00054 | final |
| baseline | 2 | 0/2 | 0.00 | 0 | 6 | 3234 | 0.00036 | final |
| decompose | 1 | 0/2 | 0.00 | 0 | 6 | 2357 | 0.00030 | final |
| decompose | 2 | 0/2 | 0.00 | 0 | 10 | 4290 | 0.00057 | final |

| arm | mean recall | mean FP | mean tokens | mean cost |
|---|---:|---:|---:|---:|
| baseline (n=2) | 0.00 | 0.00 | 3794 | $0.00045 |
| decompose (n=2) | 0.00 | 0.00 | 3324 | $0.00044 |

**Why they all failed — diagnosed from the traces:**

1. **Invented a subdirectory.** Baseline used `list_dir("")`, saw `app.py` at the
   root, then called `list_dir("app")`, `read_file("app/app.py")`,
   `read_file("app/db.py")`, `read_file("app/handlers.py")`. All returned
   `{"error":"not found"}`. It read that as "handlers.py is empty or missing" and
   concluded there were no flows, without ever calling `read_file("app.py")`.
2. **Regex into a literal grep.** Every baseline and the decomposers called
   `grep("execute|query")`, `grep("(request|body|params|query|input)\b")`, etc.
   The tool contract says grep is an **exact literal substring, not a regex**, so
   those matched nothing. Nano read empty results as "no sources, no sinks."
3. **Consequence for decomposition:** stage 1 emitted prose ("there are no
   sources or sinks"), not the requested JSON candidate list; stage 2 received
   no candidates and correctly-ish printed `NONE`. The verifier never had a decoy
   to reject, so this run says nothing yet about verifier quality.

## Guided control (supplementary, clearly labelled)

Same fixture, same programs, same prompts — only an appended tool-contract note:
grep is literal (no `|`), and files live at the root (read the names `list_dir`
returns, no `app/` prefix). This isolates "can nano reason about the taint
fixture when it actually reaches the code" from "can nano operate the tools."

| variant | run | real_flows_found | recall | false_positives | steps | put tokens | cost (USD) | stop |
|---|---:|:-:|:-:|:-:|---:|---:|---:|:--|
| baseline-guided | 1 | 0/2 | 0.00 | 0 | 9 | 6517 | 0.00070 | final |
| baseline-guided | 2 | 0/2 | 0.00 | 0 | 4 | 1502 | 0.00018 | final |
| decompose-guided | 1 | 0/2 | 0.00 | 0 | 7 | 3282 | 0.00040 | final |
| decompose-guided | 2 | 0/2 | 0.00 | 0 | 16 | 12000 | 0.00134 | final |

`decompose-guided 2` is the expensive outlier (16 steps, 12000 tokens — the
enumerate stage alone took 12 steps); it completed normally
(`stop_reason=final_completion`, no `budget_cutoff_completion`) within its
28000-token budget.

| arm | mean recall | mean FP | mean tokens | mean cost |
|---|---:|---:|---:|---:|
| baseline-guided (n=2) | 0.00 | 0.00 | 4010 | $0.00044 |
| decompose-guided (n=2) | 0.00 | 0.00 | 7641 | $0.00087 |

### Content-level adjudication (not just the FLOW-line rubric)

Only **one** guided run ever got its hands on the code: `decompose-guided 2`
(16 steps, $0.00134). It read `db.py` and `app.py` in full and found the string
interpolations. Its asserted flows were:

- `db.py:15` `executescript(f"... {msg}")` — the **F2 sink**, but it reported
  source *and* sink as `db.py:15`. It never linked the untrusted source back to
  `app.py:19` (the `X-Trace-Id` header). Not a source→sink flow.
- `db.py:26` `f"... LIMIT {count}"` — **decoy D2** (cast to `int`). It said
  "count may be untrusted" and **confirmed it**.
- `db.py:32` `"... ORDER BY %s" % col` — **decoy D3** (whitelisted in
  `handlers.py:24-25`). It said "col may be untrusted and not whitelisted" and
  **confirmed it**.

So at the content level too: **0 real flows linked**, **2 decoys confirmed as
vulnerable**. The verifier stage failed precisely the job it was added to do.

Every other run (7 of 8) reported "no flows" / `NONE`; none of them read all
three files, and none linked any `app.py` source to any `db.py` sink.

## Cost

| scope | runs | put tokens | cost (USD) |
|---|---:|---:|---:|
| primary | 4 | 14235 | $0.00177 |
| guided | 4 | 23301 | $0.00263 |
| **total** | 8 | 37536 | **$0.00440** |

Simulator cost is **$0.00**: all tool calls were served by the scenario's Lua
handlers, so no simulator model was invoked. All cost above is nano PUT tokens.

## Interpretation

- **Recall 0/2, in every arm, strict and content-level.** Not flaky — both
  baseline runs and both decomposition runs agree, and the guided control agrees.
- **False positives:** 0 under the FLOW rubric (nothing was formatted as a
  FLOW). Content-level: 2 decoys confirmed by the only run that engaged with
  the code. Either way, no evidence the decomposition kills decoys.
- **Decomposition is not worth it here.** It did not improve recall; the
  enumerate pass collapsed to prose and starved the verifier; the one verifier
  that ran confirmed the two decoys. It is also ~2× the guided cost
  ($0.00087 vs $0.00044 mean) and up to 8× a single cheap baseline run.
- **The fixture's designed subtlety (cross-file renames, header source,
  f-string/executescript, D2 cast, D3 whitelist, D4 constant sink) was largely
  never reached.** The primary experiment mostly measured "nano + a literal
  grep + an unfamiliar `cur` variable name." Even so, the guided control shows
  the taint task itself is beyond nano: when it did read the code it still
  mis-linked the source and confirmed both decoys.

### Caveats

- Scoring is FLOW-line-strict by the pinned rubric; a run that asserts a flow in
  prose but not in the `FLOW source=... sink=...` form scores 0. Content-level
  adjudication above compensates and reaches the same recall conclusion.
- The literal-grep tool contract and the `cur` (not `cursor`) variable are
  realistic but harsh for nano; they dominate the primary runs. The guided arm
  controls for this, at the cost of being a supplement rather than the requested
  protocol.
- `decompose-guided 2` is a single run; it completed normally, but is the most
expensive (12000 tokens, $0.00134).
- Small n (2 per arm). These are all-zero results, so n is not the binding
  limitation for the "can nano do it" question; it is for exact cost variance.

## Reproduce

```bash
# server on 127.0.0.1:8080 with OPEN_ROUTER_API_KEY
python3 docs/dogfood/taint-hunt-hard/register.py        # prints scenario id
python3 docs/dogfood/taint-hunt-hard/run_investigations.py <scenario_id>
python3 docs/dogfood/taint-hunt-hard/score.py           # primary
python3 docs/dogfood/taint-hunt-hard/run_guided.py <scenario_id>
python3 docs/dogfood/taint-hunt-hard/score.py docs/dogfood/taint-hunt-hard/runs_guided.json
```

Raw evidence JSON for the 8 runs is under `evidence/`; run ids are in
`runs.json` / `runs_guided.json`.

## Run IDs

| variant | run | investigation id |
|---|---|---|
| baseline | 1 | `2ea6b794-ead2-4e2c-a483-3574bb0f6160` |
| baseline | 2 | `e98d777e-5cbc-4603-b80e-9d0367b2f3ef` |
| decompose | 1 | `55ab69f0-74dd-4137-a350-b2a4e24ffd44` |
| decompose | 2 | `95ee3e50-8322-41b6-9ce1-da608c5c17f5` |
| baseline-guided | 1 | `769d0881-6631-4a26-8a1a-850c904bbae6` |
| baseline-guided | 2 | `af526a3d-b337-4006-9e37-4c3a80aa027b` |
| decompose-guided | 1 | `9969b60c-0fd7-4938-bf71-136e1231ac3f` |
| decompose-guided | 2 | `d9bed340-fea8-4e2f-9aca-d3870f476deb` |