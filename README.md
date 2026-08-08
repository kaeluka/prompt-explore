# prompt-explore

**Property-based testing for agent behavior.**

prompt-explore investigates behavioral questions about LLM prompts (the
"prompts under test", PsUT) by *executing* them in a simulated
environment: an internal LLM plays the tools and maintains world state,
while the tool searches for **reproducible witness traces** that answer
the question. Ambiguity is found by execution, not by reading.

## The UX loop

1. **Investigate.** The user provides the PsUT (prompt template,
   per-variable input specs, tool schemas, mandatory design goals) plus
   a mandatory investigation question, e.g.
   - *existential*: "are there inputs that cause destructive tool calls?"
   - *differential*: "why does this sometimes cancel, sometimes ask?"
2. **Witness.** The system generates hypotheses, builds adversarial
   scenarios, runs the PUT against simulated tools, and reports witness
   traces with instruction-level attribution. "No witness found within
   budget" is a first-class, honestly-reported outcome: every scenario
   tried is surfaced.
3. **Propose.** Ranked, **explicitly unverified** fix proposals
   (reword / split / merge / transform input data / revise design
   goals), with one-click apply in the UI.
4. **The user owns everything after.** Verification = apply a change,
   ask the same question again. The tool is the loop body of an
   interactive optimization loop; the user is the loop.

## Key design decisions

- **Question-driven**, never audit-driven: every finding is wanted by
  definition, because the user asked the question.
- **Simulation over static analysis**: a prompt template's ambiguity
  becomes obvious only when loaded with realistic data.
- **Design goals are mandatory** per prompt and are themselves
  optimization targets. Intent lives in design goals; structure lives
  in the tools array; behavior is observed in traces.
- **The judge never sees the PUT template** — verdicts must not be
  biased toward "it said it would, so it did". The judge requires the
  behavior to *actually occur* in the trace.
- **World state**: the simulator LLM proposes, code holds the truth.

## Repo layout

Cargo workspace:

```
core/            the library — all logic lives here, usable standalone
├── src/model/       pure data layer (no I/O): input → predicate → simulation → output
├── src/llm/         LlmClient trait + OpenAI-compatible adapter (z.ai, OpenRouter) + mock
├── src/simulate/    runner (PUT tool loop) + tool-simulator LLM + world state
├── src/judge/       predicate evaluation over traces (sees scenario + design goals, not the PUT)
├── src/generate/    hypothesize → scenario building → search → propose → apply
└── examples/        smoke, live_run, investigate_live, judge_live
server/          thin axum wrapper — HTTP API + web UI. No business logic.
├── src/main.rs      job-based API: POST /api/investigations → poll GET /api/investigations/:id;
│                    POST /api/apply (LLM rewrite + deterministic diff)
└── static/          web UI (structured PUT form, budget controls, transcripts, apply buttons)
```

## Status

### ✅ Done

- ✅ Full single-iteration data model with serde contract tests
- ✅ Extensible per-variable input specs (constant / NL description /
  examples — mixable within one prompt)
- ✅ `LlmClient` trait + async-openai adapter (z.ai coding plan,
  OpenRouter) + scripted `MockLlmClient`
- ✅ Runner: PUT tool loop, simulator-LLM tool responses, world-state
  bookkeeping, budget stop conditions
- ✅ Judge: NL-only LLM judge (behavior must actually occur; judge is
  blind to the PUT template). Structural checks were tried and dropped.
- ✅ Hypothesis generation + adversarial scenario builder + search
- ✅ Witness reporting: every attempt surfaced, including negative results
- ✅ Proposal generation + apply (the LLM rewrites the target field,
  a deterministic word-level diff is computed for review; goal_revision
  edits goals, everything else edits the template — never both)
- ✅ HTTP server + web UI (job-based API, transcripts, budget controls,
  apply buttons)
- ✅ CLI-style live runs via examples

### ⬜ Not done yet

- ⬜ Differential questions (witness pairs)
- ⬜ Attribution via instruction ablation
- ⬜ Parallel trace execution
- ⬜ Witness fixtures: replay, regression corpus, re-run semantics
  (the "ask the same question again" loop)
- ⬜ Dedicated simulated-user model (persona, consistency across
  turns — basic user replies already work via a tool whose description
  says it responds with the user's answer)
- ⬜ Multi-prompt pipelines (topology is described but read nowhere;
  runs target one PUT)
- ⬜ PsUT version history + rollback (diffs exist; commits/undo don't)
- ⬜ PsUT optimizer agent: an in-process agentic loop over core/
  (investigate + free prompt rewrites as discrete, diffed, versioned
  edits; design-goal edits gated on human approval)

## Build / test / run

All commands run from the workspace root. Live runs need a z.ai coding-plan
API key in `ZAI_API_KEY` (the server refuses to start without it);
`cargo test` is fully deterministic and needs no key.

```bash
cargo test                                           # deterministic, no API key needed
ZAI_API_KEY=... cargo run -p prompt-explore-server   # web UI on http://127.0.0.1:8080
ZAI_API_KEY=... cargo run --example investigate_live # CLI-style live investigation
ZAI_API_KEY=... cargo run --example smoke            # minimal provider check
```

Live runs use the z.ai coding-plan endpoint (model `glm-5.2`).
OpenRouter is supported via `OpenAiCompatibleClient::openrouter`.

## Contributing

See [AGENTS.md](AGENTS.md) — notably the **dogfooding rule**: when you
change any of the tool's own prompts (judge, simulator, hypothesizer,
scenario builder, proposal generator), run the tool against itself and
record the before/after result in the commit message.
