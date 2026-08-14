# AGENTS.md

Guidance for AI agents (and humans) working in this repository.

## What this is

**prompt-explore** — property-based testing for agent behavior. A user
supplies scenarios (author-supplied world narratives) and a prompt under
test (PUT); the tool runs every scenario inside its simulated world and
returns the complete evidence — the world, the input domain, the resolved
inputs, and the full trace of steps. **The caller is the judge:** there
is no in-harness verdict. The user may also state a free-form `reason`
for the run (what it aims to accomplish, what changed compared to runs
before, what a reader should know — no strict standard), but it is
advisory framing — surfaced with the result to guide reading the traces
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
  The job store is **in memory** — jobs (and any caller-supplied annotations on
  them) are lost on restart. This is a conscious limitation, chosen to keep
  development fast and deployment trivially easy (single binary, no DB). Do not
  add a persistence layer casually; if durability becomes a requirement, it
  should be a deliberate design decision with its own scope — not a drive-by.

## Conventions

- All LLM access goes through the `LlmClient` trait (`core/src/llm/`). Runtime
  layers never depend on a concrete provider; tests use `MockLlmClient` with
  scripted responses — keep tests deterministic, no network.
- **The caller is the judge.** The harness runs scenarios and surfaces traces
  (world, input domain, resolved inputs, full steps); it produces no verdict.
  Nothing is judged against the (optional) `reason` — it is advisory framing
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

## Releasing

**Releases are tag-driven: push a `v*` tag, CI builds the binaries, a
GitHub Release gets the assets.** No release artifact is hand-built or
hand-uploaded. The workflow (`.github/workflows/release.yml`) compiles
`prompt-explore-server` for five targets — linux x86_64/ARM64 (musl, fully
static), macOS x86_64/ARM64, windows x86_64 — packages each, and attaches
the archives to the release for that tag. End users grab the bin directly;
no Rust toolchain required.

**To cut a release:**

**REQUIRED: clarify the version before bumping.** When asked to cut a
release, do NOT choose the version number yourself — ask the operator
which bump they want (patch / minor / major, or an explicit number) and
confirm before committing. Version bumps encode intent (breaking vs
additive vs fix) and that judgment belongs to the operator, not the
agent. Only proceed unilaterally if the operator already named the
version in the request.

```bash
# 1. main is green, working tree clean.
cargo test
git status   # clean

# 2. Bump the version. It lives once, in the workspace manifest.
#    Cargo.toml:  [workspace.package]  version = "0.1.1"
#    (after the operator has confirmed WHICH bump)

# 3. Commit, tag that commit, push both.
git commit -am "Bump version to 0.1.1"
git tag v0.1.1
git push origin main
git push origin v0.1.1
```

Pushing the tag triggers the workflow. Watch the Actions tab; once all five
build jobs pass, the release job attaches the archives to the `v0.1.1`
release (notes are auto-generated from commits/PRs since the last tag).

**For a first release or any risky cut, ship it as a prerelease.** In the
release UI check "Set as a pre-release", verify the binaries download and
run on each platform, then uncheck it and set it as Latest. A bad build then
never becomes someone's first impression, and you avoid delete-and-retry.

**Releases are not permanent.** You can delete a release (its page +
attached assets) and delete/recreate the tag to re-cut, or edit a release
to swap individual bad assets without nuking the whole thing.

**Dry-run a matrix or workflow change without publishing.** The workflow
has a `workflow_dispatch` trigger: Actions tab → "release" → Run workflow.
It builds every target and exposes them as workflow artifacts, but skips
the publish step (which runs only on a real `v*` tag). Verify a new target
or an edit there before committing to a tag.

**No secrets are involved.** The publish step uses GitHub's auto-injected
`GITHUB_TOKEN` with `contents: write`; the build is a pure `cargo build`,
so no provider API keys are needed at build time (those are runtime
concerns for whoever runs the binary).

**Every third-party action in the workflow is pinned to an immutable commit
SHA** (the ref it came from is a trailing comment), so a compromised upstream
tag cannot change what runs in CI. Bumping a pinned SHA is a deliberate edit
— never swap a SHA back for a moving `@vN` ref.

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

**The `openapi.json` spec is a prompt too — it is in scope.** An agent
caller's ONLY documentation is the spec; to that reader it literally IS
the system prompt. So when you change any endpoint description, schema
description, or example in the spec:

- **Probe it like a prompt.** Feed the spec verbatim to an LLM as the
  manual ("this is your only documentation") and ask realistic caller
  questions — "I want to find out whether X ever happens; walk me
  through your calls", "this just happened: <state or error>; what do
  you do next?" Read the answers for stumbles: wrong endpoint sequences,
  invented affordances, missed affordances, or "the spec does not say"
  where it should.
- **Before/after, same probes.** Re-run the same probe set after the
  edit and compare. The goal is the same as for any prompt: the
  caller-model's planned behavior (the requests it would send) gets
  closer to correct.
- **Record the finding** in the commit message like any other dogfood
  result. Probe answers that invent or miss affordances are spec bugs:
  fix the spec's words, not the prober's model.

**Framing probes takes iteration — the framing IS the experiment.** A
probe that asks "how would you grade traces?" gets plans; plans are
cheap and every model aces them. The signal came only when the probe
was framed as: "you are a coding agent with bash access; you triggered
an investigation to <goal>; it is done; GET /api/investigations/$id
returns ~100kb — grade it on <axis>." Then the temptation is real: the
shell is one keystroke away, the data is too big to read, and the
probe measures what the agent actually does (curl|jq the structure?
grep for keywords? read the traces?) rather than what it says it
would do. Expect to iterate on framing several times before the probe
actually probes; when the answer stops being a plan and starts being
behavior, the framing is right.

**When the probe world simulates tool access to the API itself, give
the WORLD the real spec.** A scenario whose PUT has a bash tool and
whose user message points at `127.0.0.1:8080` makes the simulator
answer `curl /api/investigations/{id}` from imagination — and it will
invent plausible-but-wrong API behavior (observed: a fictional
`409 job_read_only` on PATCH, a `grades` map with string values the
real schema forbids). Those inventions then confound the probe's
conclusion. Paste the real openapi.json into the world (or its
relevant slices) and pin the rendered responses to it, so simulated
API behavior matches the spec under test.

**Verified technique (spec-in-world, A/B, glm-5.2):** same three
bash-grading probes, two worlds — one with the simulator answering
`curl` from imagination, one embedding the real openapi.json with
"the spec is authoritative for every rendered response" pinned.

- Without the spec: PATCH grades blocked by invented `409`/`405`
  read-only errors (0/3 recorded); the job view rendered with an
  off-schema attempt shape that broke the agent's jq, costing it
  steps to debug the simulator's fiction.
- With the spec: PATCH succeeded and echoed the documented response
  (grades persisted, re-GET confirmed); the view was spec-shaped so
  jq navigated it on the first try; and when the sim still garbled
  one `ls`, the agent caught it, discarded it, re-derived from raw
  JSON, and flagged the discard in its answer.

The embedded spec upgrades the simulated API from plausible-fiction
to contract — the probe then measures the agent, not the sim's
inventions.

Example precedent: tightening the (now-removed) judge prompt to require that
a behavior *actually occurred* was validated by re-running the
destructive-action investigation — the false positive vanished and a real
witness was found instead. The same loop now applies to the simulator: when
you change how the world is rendered, re-run and read the traces. And to the
spec: probe answers that invent or miss affordances mean the spec's words
(not the prober's model) need fixing.
