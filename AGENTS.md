# AGENTS.md

Guidance for AI agents (and humans) working in this repository.

## What this is

**prompt-explore** — property-based testing for agent behavior. A user
supplies scenarios (author-supplied world narratives) and a prompt under
test (PUT); the tool runs every scenario inside its simulated world and
returns the complete evidence — the world, the input domain, the resolved
inputs, and the full trace of steps. **The caller is the judge:** there
is no in-harness verdict. The user may also state a behavioral question
about the PUT, but it is advisory framing — it states what the caller is
worried about and is surfaced with the result to guide reading the traces
— not an oracle. Traces are informative even when nothing is obviously
wrong. The user owns everything after the run — the tool is the loop body
of an interactive optimization loop, the user is the loop.

**The traces are the deliverable; the fix is the caller's job.** Earlier
the tool had an in-harness LLM judge that produced a verdict (matched /
witness) per trace, and before that it proposed unverified fix suggestions
it could apply. Both were removed. The verdict was removed because the
question-as-oracle is the hardest, most fragile input — it forces you to
predict the failure you want to discover — and the caller (a human or an
LLM session driving the API) holds far richer intent than the question
encodes. So the harness's job is to run scenarios and surface complete
evidence; the caller reads the traces and judges, then finds and owns the
fix. (Fix suggestions also only ever covered a single prompt; real prompt
optimization often spans interacting prompts. If they return, they will be
built for the multi-prompt case, not single-PUT.)

## Design philosophy

**prompt-explore is a thin harness around an LLM. Nothing more.**

The LLM does the semantic work — inventing scenarios, simulating tool
responses and users. The harness does only the deterministic bookkeeping
it can do better than an LLM: routing messages, validating argument
shapes, applying state patches, counting budget, computing diffs. The
split is the point: deterministic things in code, semantic things in the
model. Judging the resulting traces is the CALLER's semantic work — the
harness does not do it.

**We chose LLMs knowing they are imperfect, and we accept it.** A simulator
asked to break a tool will sometimes make it work; a tool response will
sometimes drift from the narrative. This is a property of the approach,
not a defect to patch with a second system. The caller reads the traces
and catches it.

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
that deviation is visible in the trace and the caller — who sees the same
narrative and trace — can catch it. The user is the loop; they see what
happened, not what was supposed to happen.

**Every LLM phase is an observable status.** An investigation's LLM work is
the per-scenario PUT tool loop. `GET /api/investigations/{id}` must report
which phase the job is in (and the UI must show it), never a bare
"running". (With the judge removed, the only phase is `scenarios`; the
contract is kept so a future phase — e.g. a caller-supplied judge — slots
in without re-plumbing the status surface.)

**Environments are narratives, not data.** A scenario is a world
*specification* — facts, completeness assertions, rendering instructions —
never an instantiated environment. The simulator lazily renders concrete
tool responses from the narrative. Materializing an environment requires a
closed world (enumerable, bounded, copyable); open worlds — web search,
email, a payment network — can never be materialized, so narratives are the
only mechanism that generalizes. Completeness assertions are total for
closed worlds ("these are ALL the entry points") and scoped for open ones
("these are the relevant results on this topic"). **Inputs are described, not supplied.** A scenario carries an `input_domain`
— a per-`{{variable}}` description (value space, semantics, preconditions/
trust contracts). Finding the concrete input value is the simulator's job:
it picks one from the domain, fills the template, and the chosen value is
reported in the trace's `resolved_inputs` so a trace is reproducible.
This is the property-based-testing move — describe the domain, sample it.

**A scenario is a value, not a record.** It carries no identity (`id`):
it is `(world, input_domain, user_message)` and the run output embeds the
scenario *by value* per attempt, never by id or index. Correlation is
content-equality.

**The consumer owns simulation quality.** Whoever consumes an
investigation's output judges whether the tool simulation was good enough.
If it wasn't, the remediation is a user action — sharpen the scenario and
re-investigate — not harness machinery. The harness's job ends at
transparency: surface the narrative, the trace, and divergence signals
(e.g. a tool response that contradicts the stated facts — the caller,
reading the trace against the narrative, flags it; there is no in-harness
judge to do so). Do not build consistency
machinery (materialized fixtures, render caches) to make fidelity the
harness's problem. This is "the user is the loop" extended one level down —
same loop, new object — and it is the contract a future optimizer agent
operates under.

**Scenarios are authored outside the harness.** There is deliberately no
scenario-generation endpoint and no generation-on-submit: an optional
`scenarios` field with an "absent means generate" default is *easy, not
simple* — one endpoint, one contract. The operator's agent (e.g. Claude)
writes scenarios; the harness evaluates them. When authoring a scenario,
the world is the ground truth and must pin four things (all NL, all
visible to the simulator and the caller who reads the trace):

1. **Inventory** — what exists and where, covering every query type the
   PUT's tools allow (files/paths for a repo; orders and their states for
   a support agent; per-topic results for a search tool).
2. **Facts** — including *negative* facts (what does NOT exist, what NEVER
   happens). LLMs default to inventing positive content; absences must be
   stated. Often the negative facts are what make a trace decidable.
3. **Completeness assertions** — "these are ALL the entry points" (closed
   world) or "these are the relevant results on this topic" (open world).
4. **Rendering rules** — refuse queries outside the inventory; filler
   introduces no new facts; never contradict the facts.

Size the world to the investigation's step budget: a small world fully
explored beats a large world half-explored. And vary the worlds across a
corpus — same-shape scenarios prove the same thing twice.

**The API is self-explanatory to an agent.** An agent reading only
`openapi.json` must be able to use the API correctly — without reading the
codebase. Schema and endpoint descriptions therefore carry *concepts*, not
just field names: what a scenario IS (a test case: a world spec plus a
protagonist) and what it is FOR. When you change the API, write for that
reader.

## Repo layout

Cargo workspace:

- `core/` — the library. All logic lives here and must stay usable standalone
  (lib / CLI / examples). Pure model layer (`model/`), LLM abstraction (`llm/`),
  simulation (`simulate/`), and the investigation orchestrator (`generate/`).
  There is no judge module — the caller is the judge.
- `server/` — thin axum wrapper (HTTP + web UI). **No business logic here.**
  Job-based API (`POST /api/investigations` → poll `GET /api/investigations/:id`).

## Conventions

- All LLM access goes through the `LlmClient` trait (`core/src/llm/`). Runtime
  layers never depend on a concrete provider; tests use `MockLlmClient` with
  scripted responses — keep tests deterministic, no network.
- **The caller is the judge.** The harness runs scenarios and surfaces traces
  (world, input domain, resolved inputs, full steps); it produces no verdict.
  Nothing is judged against the (optional) `question` — it is advisory framing
  for whoever reads the traces. `design_goals` on the PUT are documentation
  the caller reads, not something enforced during a run.
- Negative results are first-class: surface what was tried (scenarios, traces,
  failures), never just "nothing found".
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

Live runs go through the `genai` multi-provider library (`ProviderClient`
behind the `LlmClient` trait). Provider is chosen per call by the model
namespace: `zai_coding::glm-5.2` (default; `ZAI_API_KEY`),
`open_router::<model>` (`OPEN_ROUTER_API_KEY`), `bedrock_sigv4::<model-id>`
(default AWS credential chain — `aws sso login` works). A bare model name
uses the server's default provider (`PROMPT_EXPLORE_PROVIDER`, default
`zai`).

## Dogfooding rule

**Whenever you optimize a prompt, use the opportunity for dogfooding.**

This tool *is* a prompt-optimization tool, and its own prompts (notably
the tool simulator) are its most important internal artifacts. So when you
change any of them:

1. **Run the tool against itself.** Use prompt-explore (the server or a live
   example) to investigate the behavior your prompt change targets. Don't just
   reason about whether the new prompt is better — run the scenarios and read
   the traces that show it.
2. **Before/after, same scenarios.** Re-run the same investigation and
   compare the traces (e.g., did the false behavior disappear? did the
   realistic behavior survive?). If you changed the simulator, check that
   scenarios still produce meaningful traces.
3. **Record the finding.** Mention the dogfood result in the commit message
   (what was investigated, what the outcome was).

Example precedent: tightening the (now-removed) judge prompt to require that
a behavior *actually occurred* was validated by re-running the
destructive-action investigation — the false positive vanished and a real
witness was found instead. The same loop now applies to the simulator: when
you change how the world is rendered, re-run and read the traces.
