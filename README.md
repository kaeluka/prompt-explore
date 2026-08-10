# prompt-explore

**Property-based testing for agent behavior.**

prompt-explore investigates behavioral questions about LLM prompts (the
"prompts under test", PUTs) by *executing* them in simulated
environments: an internal LLM plays the tools, rendering each world and
picking concrete inputs from an author-supplied scenario. The tool reports
**every trace** — the complete evidence (world, input domain, resolved
inputs, full steps). **The caller is the judge:** there is no in-harness
verdict. Ambiguity is found by execution, not by reading.

## The UX loop

1. **Author scenarios.** The user (typically via their AI agent) writes
   scenarios: natural-language world specifications — inventory, facts
   (including negatives), completeness assertions, rendering rules —
   plus an opening user message. The harness deliberately does not
   generate them (see AGENTS.md for authoring guidance).
2. **Investigate.** The user provides the PUT (prompt template,
   per-variable input specs, tool schemas, design goals), the scenarios,
   and an optional investigation question, e.g.
   - *existential*: "are there inputs that cause destructive tool calls?"
   - *differential*: "why does this sometimes cancel, sometimes ask?"
   The question is advisory framing — it states what the caller is
   worried about — not an oracle the harness enforces.
3. **Traces.** The system runs the PUT against the simulated worlds and
   reports every trace. "Every scenario completed" is a first-class,
   honestly-reported outcome: every scenario tried is surfaced.
4. **Traces are the deliverable.** The traces (world, input domain,
   resolved inputs, full steps) are the output. **prompt-explore does not
   judge or propose fixes** — the caller reads the traces and judges, then
   finds the fix themselves; the hard part is producing the traces.
5. **The user owns everything after.** Verification = edit the prompt,
   re-run the same scenarios. The tool is the loop body of an
   interactive optimization loop; the user is the loop.

## Key design decisions

- **Question-driven, never audit-driven**: every finding is wanted by
  definition, because the user asked the question. (The question is now
  *advisory* — it frames what the caller is worried about; the harness
  no longer judges against it.)
- **Simulation over static analysis**: a prompt template's ambiguity
  becomes obvious only when loaded with realistic data.
- **Design goals are mandatory** per prompt and are themselves
  optimization targets for the caller. Intent lives in design goals;
  structure lives in the tools array; behavior is observed in traces.
- **The caller is the judge** — the harness runs scenarios and surfaces
  traces; it produces no verdict. (An earlier in-harness LLM judge was
  removed: the question-as-oracle is the hardest, most fragile input,
  and the caller holds far richer intent.)
- **World state**: the simulator LLM proposes, code holds the truth.

## Repo layout

Cargo workspace:

```
core/            the library — all logic lives here, usable standalone
├── src/model/       pure data layer (no I/O): input → simulation → output
├── src/llm/         LlmClient trait + OpenAI-compatible adapter (z.ai, OpenRouter) + mock
├── src/simulate/    runner (PUT tool loop) + tool-simulator LLM + world state + transcript rendering
├── src/generate/    run orchestration (the investigation driver). No judge — the caller is the judge.
└── examples/        smoke, live_run, investigate_live
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
  library (z.ai coding-plan/standard, OpenRouter, Baseten, AWS Bedrock
  Converse+SigV4, plus any OpenAI-format endpoint) + scripted
  `MockLlmClient`
- ✅ Runner: PUT tool loop, simulator-LLM tool responses, world-state
  bookkeeping, budget stop conditions
- ✅ Scenario execution (author-supplied world + input domain) → traces
- ✅ Trace reporting: every attempt surfaced, including negative results
- ✅ Per-role models: PUT and simulator independently selectable
  (`model` / `sim_model`), across providers. (The judge role was removed —
  the caller is the judge.)
- ✅ HTTP server + web UI (job dashboard, live traces, observable LLM phases,
  budget controls)
- ✅ CLI-style live runs via examples

### ⬜ Not done yet

- ⬜ Differential questions (comparing traces across scenario variants)
- ⬜ Attribution via instruction ablation
- ⬜ Parallel trace execution
- ⬜ Trace fixtures: replay, regression corpus, re-run semantics
  (the "run the same scenarios again" loop)
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
`ZAI_API_KEY` (z.ai), `OPEN_ROUTER_API_KEY` (OpenRouter),
`BASETEN_API_KEY` (Baseten), the default
AWS credential chain for Bedrock (`aws sso login`, profiles, IMDS), or
GCP Application Default Credentials for Gemini via Vertex AI
(`gcloud auth application-default login`).
The provider is chosen per call by the model namespace —
`zai_coding::glm-5.2`, `zai::glm-4.6`, `open_router::deepseek/...`,
`bedrock_sigv4::<model-id>`, `baseten::<model-id>`,
`vertex::gemini-2.5-pro`; a bare name (e.g.
`glm-5.2`) uses the server's default provider (`PROMPT_EXPLORE_PROVIDER`,
default `zai`).

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
OpenRouter, Baseten, AWS Bedrock (native Converse + SigV4, full AWS
credential chain), and Google Gemini via Vertex AI (GCP Application
Default Credentials) are first-class.

## Contributing

See [AGENTS.md](AGENTS.md) — notably the **dogfooding rule**: when you
change any of the tool's own prompts (notably the simulator), run the tool
against itself and record the before/after result in the commit message.
