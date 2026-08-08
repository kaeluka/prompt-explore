# AGENTS.md

Guidance for AI agents (and humans) working in this repository.

## What this is

**prompt-explore** — property-based testing for agent behavior. A user states a
behavioral question about a prompt under test (PUT); the tool searches simulated
scenarios for a witness trace, attributes the behavior, and proposes *unverified*
fixes. The user owns everything after the run — the tool is the loop body of an
interactive optimization loop, the user is the loop.

## Design philosophy

**prompt-explore is a thin harness around an LLM. Nothing more.**

The LLM does the semantic work — hypothesizing how a behavior could arise,
inventing scenarios, simulating tool responses and users, judging traces,
proposing fixes. The harness does only the deterministic bookkeeping it can
do better than an LLM: routing messages, validating argument shapes, applying
state patches, counting budget, computing diffs. The split is the point:
deterministic things in code, semantic things in the model.

**We chose LLMs knowing they are imperfect, and we accept it.** A simulator
asked to break a tool will sometimes make it work. A judge will sometimes
misread a trace. This is a property of the approach, not a defect to patch
with a second system.

**Resist building a parallel deterministic system.** The recurring temptation
is to "compile" natural-language intent into a DSL, or to "enforce" a
condition in code so the model can't get it wrong. Any DSL you design will
fail to express a realistic case; you'll extend it, hit compilation bugs,
and debug them — swapping LLM flakiness (already accepted) for harness bugs
(now your problem), none of it closer to the core mission. When in doubt,
pass the user's words through to the model as-is.

**The answer to LLM unreliability is transparency, not enforcement.** Surface
everything: every scenario tried, every trace, the stated environment, the
world state. When the simulator deviates from what the operator specified,
that deviation is visible in the trace and the judge (which sees the same
context) can catch it. The user is the loop; they see what happened, not what
was supposed to happen.

**Environments are narratives, not data.** A scenario is a world
*specification* — facts, completeness assertions, rendering instructions —
never an instantiated environment. The simulator lazily renders concrete
tool responses from the narrative. Materializing an environment requires a
closed world (enumerable, bounded, copyable); open worlds — web search,
email, a payment network — can never be materialized, so narratives are the
only mechanism that generalizes. Completeness assertions are total for
closed worlds ("these are ALL the entry points") and scoped for open ones
("these are the relevant results on this topic"). The world-state mechanism
(mutations during a trace) composes with this: the narrative says what
exists, world state tracks what the trace changed.

**The consumer owns simulation quality.** Whoever consumes an
investigation's output judges whether the tool simulation was good enough.
If it wasn't, the remediation is a user action — sharpen the scenario and
re-investigate — not harness machinery. The harness's job ends at
transparency: surface the narrative, the trace, and divergence signals
(e.g. the judge flagging "the simulator contradicted the stated facts",
kept separate from "the PUT misbehaved"). Do not build consistency
machinery (materialized fixtures, render caches) to make fidelity the
harness's problem. This is "the user is the loop" extended one level down —
same loop, new object — and it is the contract a future optimizer agent
operates under.

## Repo layout

Cargo workspace:

- `core/` — the library. All logic lives here and must stay usable standalone
  (lib / CLI / examples). Pure model layer (`model/`), LLM abstraction (`llm/`),
  simulation (`simulate/`), judging (`judge/`), generation + search (`generate/`).
- `server/` — thin axum wrapper (HTTP + web UI). **No business logic here.**
  Job-based API (`POST /api/investigations` → poll `GET /api/investigations/:id`).

## Conventions

- All LLM access goes through the `LlmClient` trait (`core/src/llm/`). Runtime
  layers never depend on a concrete provider; tests use `MockLlmClient` with
  scripted responses — keep tests deterministic, no network.
- The judge sees the scenario and design goals but **not** the PUT template
  (verdicts must not be biased toward "it said it would, so it did").
- Proposals are always explicitly unverified; the confidence note must say so
  and instruct the user to re-ask the question to check.
- Negative results are first-class: surface what was tried (scenarios, traces,
  verdicts), never just "nothing found".
- The OpenAPI spec is generated, not hand-written: handlers and
  request/response types carry `utoipa` annotations, and `openapi.json` is
  compiled from them. **Whenever the API changes** (endpoints, request or
  response shapes), re-run `scripts/dump-openapi.sh` and commit the updated
  `openapi.json` + `API.md` together with the change.

## Build / test / run

```bash
cargo test                                  # all unit + integration tests
cargo build -p prompt-explore-server        # the server
ZAI_API_KEY=... cargo run -p prompt-explore-server   # serve on 127.0.0.1:8080
ZAI_API_KEY=... cargo run --example investigate_live # CLI-style live run
```

Live runs use the z.ai coding-plan endpoint (model `glm-5.2`); OpenRouter is
also supported via `OpenAiCompatibleClient::openrouter`.

## Dogfooding rule

**Whenever you optimize a prompt, use the opportunity for dogfooding.**

This tool *is* a prompt-optimization tool, and its own prompts (judge, tool
simulator, hypothesizer, scenario builder, proposal generator) are its most
important internal artifacts. So when you change any of them:

1. **Run the tool against itself.** Use prompt-explore (the server or a live
   example) to investigate the behavior your prompt change targets. Don't just
   reason about whether the new prompt is better — get a witness or a
   no-witness result that shows it.
2. **Before/after, same question.** If you changed the judge, re-run the same
   investigation and compare outcomes (e.g., did the false positive disappear?
   did recall survive?). If you changed the simulator or generators, check that
   scenarios still produce meaningful traces.
3. **Record the finding.** Mention the dogfood result in the commit message
   (what was investigated, what the outcome was).

Example precedent: tightening the judge prompt to require that a behavior
*actually occurred* was validated by re-running the destructive-action
investigation — the false positive vanished and a real witness (preemptive
"yes" treated as confirmation) was found instead.
