# prompt-explore

**Property-based testing for agent behavior.**

prompt-explore investigates behavioral questions about LLM prompts (the
"prompts under test", PsUT) by *executing* them in simulated
environments: an internal LLM plays the tools and maintains world state,
rendering each world from an author-supplied scenario narrative. The
tool reports **reproducible witness traces** that answer the question.
Ambiguity is found by execution, not by reading.

## The UX loop

1. **Author scenarios.** The user (typically via their AI agent) writes
   scenarios: natural-language world specifications — inventory, facts
   (including negatives), completeness assertions, rendering rules —
   plus an opening user message. The harness deliberately does not
   generate them (see AGENTS.md for authoring guidance).
2. **Investigate.** The user provides the PUT (prompt template,
   per-variable input specs, tool schemas, mandatory design goals), the
   scenarios, and a mandatory investigation question, e.g.
   - *existential*: "are there inputs that cause destructive tool calls?"
   - *differential*: "why does this sometimes cancel, sometimes ask?"
3. **Witness.** The system runs the PUT against the simulated worlds,
   judges every trace, and reports witness traces. "No witness found" is
   a first-class, honestly-reported outcome: every scenario tried is
   surfaced.
4. **Witness is the deliverable.** The witness + trace + verdict +
   incidental findings are the output. **prompt-explore does not propose
   fixes** — knowing the witness and trace, the caller finds the fix
   themselves; the hard part is finding the witness. (Fix suggestions
   were removed: they only ever covered a single prompt, and real
   optimization often spans interacting prompts. If they return, they
   will target the multi-prompt case.)
5. **The user owns everything after.** Verification = edit the prompt,
   re-run the same scenarios. The tool is the loop body of an
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
├── src/generate/    run + judge orchestration (the investigation driver)
└── examples/        smoke, live_run, investigate_live, judge_live
server/          thin axum wrapper — HTTP API + web UI. No business logic.
├── src/main.rs      job-based API: POST /api/investigations → poll GET /api/investigations/:id
└── static/          web UI (job dashboard, live traces, transcripts)
```

## Status

### ✅ Done

- ✅ Full single-iteration data model with serde contract tests
- ✅ Extensible per-variable input specs (constant / NL description /
  examples — mixable within one prompt)
- ✅ `LlmClient` trait + multi-provider client built on the `genai`
  library (z.ai coding-plan/standard, OpenRouter, AWS Bedrock
  Converse+SigV4, plus any OpenAI-format endpoint) + scripted
  `MockLlmClient`
- ✅ Runner: PUT tool loop, simulator-LLM tool responses, world-state
  bookkeeping, budget stop conditions
- ✅ Judge: NL-only LLM judge (behavior must actually occur; judge is
  blind to the PUT template). Structural checks were tried and dropped.
- ✅ Scenario execution (author-supplied narratives) + judge
- ✅ Witness reporting: every attempt surfaced, including negative results
- ✅ Per-role models: PUT, simulator, and judge independently selectable
  (`model` / `sim_model` / `judge_model`), across providers
- ✅ Advisory design-goal findings (`incidental_findings`) — surfaced, never
  mixed into the witness verdict
- ✅ HTTP server + web UI (job dashboard, live traces, observable LLM phases,
  budget controls)
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
- ⬜ Per-role model selection: pick the model that *simulates tools*
  separately from the model that *runs the PUT conversation*. Important
  for cost optimisation — finding the cheapest model that still works
  requires keeping a decent model as the simulator (it has to roleplay
  a believable environment), even while the PUT runs on a cheap one.
- ⬜ Multi-prompt scenarios: a dedicated endpoint that uses
  single-prompt investigations as a *tool* to explore pipelines
  (e.g. one agent's output feeding another's input). The API is
  deliberately single-PUT; premature multi-prompt plumbing was removed.
- ⬜ PsUT version history + rollback (diffs exist; commits/undo don't)
- ⬜ PsUT optimizer agent: an in-process agentic loop over core/
  (investigate + free prompt rewrites as discrete, diffed, versioned
  edits; design-goal edits gated on human approval)

## Build / test / run

All commands run from the workspace root. `cargo test` is fully
deterministic and needs no key. Live runs authenticate per provider:
`ZAI_API_KEY` (z.ai), `OPEN_ROUTER_API_KEY` (OpenRouter), or the default
AWS credential chain for Bedrock (`aws sso login`, profiles, IMDS).
The provider is chosen per call by the model namespace —
`zai_coding::glm-5.2`, `zai::glm-4.6`, `open_router::deepseek/...`,
`bedrock_sigv4::<model-id>`; a bare name (e.g. `glm-5.2`) uses the
server's default provider (`PROMPT_EXPLORE_PROVIDER`, default `zai`).

```bash
cargo test                                           # deterministic, no API key needed
ZAI_API_KEY=... cargo run -p prompt-explore-server   # web UI on http://127.0.0.1:8080
ZAI_API_KEY=... cargo run --example investigate_live # CLI-style live investigation
ZAI_API_KEY=... cargo run --example smoke            # minimal provider check
OPEN_ROUTER_API_KEY=... cargo run --example smoke -- openrouter deepseek/deepseek-v4-flash-0731
scripts/dump-openapi.sh                              # regenerate openapi.json + API.md from the code
```

The OpenAPI spec is compiled from the server's annotated handlers and
types (utoipa), so it cannot drift from the implementation. It is also
served by a running server at `GET /api/openapi.json`. `API.md` is a
human-readable rendering of the same spec; both are regenerated by the
script.

LLM access goes through the `genai` multi-provider library behind our
`LlmClient` trait, so any provider genai supports (26+) works — z.ai,
OpenRouter, and AWS Bedrock (native Converse + SigV4, full AWS credential
chain) are first-class.

## Contributing

See [AGENTS.md](AGENTS.md) — notably the **dogfooding rule**: when you
change any of the tool's own prompts (judge, simulator), run the tool
against itself and record the before/after result in the commit message.
