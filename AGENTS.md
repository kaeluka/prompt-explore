# AGENTS.md

Guidance for AI agents (and humans) working in this repository.

## What this is

**prompt-explore** — property-based testing for agent behavior. A user states a
behavioral question about a prompt under test (PUT); the tool searches simulated
scenarios for a witness trace, attributes the behavior, and proposes *unverified*
fixes. The user owns everything after the run — the tool is the loop body of an
interactive optimization loop, the user is the loop.

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
