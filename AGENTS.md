# AGENTS.md

Guidance for agents and humans working on **prompt-explore**.

## Product: one shared workspace, two interfaces

**The core product is the API. The API is for the agent; the UI is for the
human. They represent the same shared workspace.**

prompt-explore is property-based testing for agent behavior. A caller supplies
one reusable scenario and one system under test per investigation. The system
is a Lua orchestration program; a single-prompt shorthand uses the default
program. The harness executes it against the scenario and returns complete
evidence: the world narrative, input domain, resolved inputs, model turns, tool
responses, execution history, budgets, accounting and failures.

An agent uses the API to author scenarios, probe simulations, run investigations,
inspect evidence, record assessments and compare results. The human uses the UI
to see and understand that work, discuss it with the agent, and direct what
happens next. These are not separate products or separate accounts of events.

**The UI is a communication interface, not decoration.** If the agent can see a
stage budget through the API but the human cannot find it in the stage view,
they no longer share an understanding of the experiment. Silent omissions,
misleading hierarchy and ambiguous labels cause friction, misunderstandings and
bad decisions. Users experience that as “prompt-explore doesn't really work,”
even when the underlying execution is correct. Treat these as product correctness
bugs, not cosmetic shortcomings.

**Good typography, visual design and UX are core features, not polish.** The UI
is how a human reads evidence and makes decisions. Reading order, hierarchy,
legibility, labeling and interaction are part of whether the product works.
“The data is technically present” is not a pass. A correct execution that a
human cannot read well is a communication failure.

**Completeness of information is equally a core feature.** Good design must
never be achieved by hiding, omitting or simplifying away evidence. The two are
not in tension: a well-designed abstraction makes the complete picture easier
to grasp, while honest disclosure keeps every recorded fact reachable. An
interface that is beautiful but lossy fails as surely as one that is complete
but unreadable.

**If the human doesn't need to see it, neither does the agent.** Information
parity cuts both ways. The API is not allowed to accumulate fields, statuses or
controls that no human would ever need in order to understand or steer the work;
the agent must not have access to a richer world than the person it works for.
Before exposing something, ask what human decision it informs. If nothing needs
it, it should not be recorded, returned or surfaced to either interface. This
keeps the shared workspace genuinely shared: one set of facts, meaningful and
visible to both sides.

**The UI's structure and names should mirror the API's.** The same facts are
organized the same way and called by the same words in both interfaces: a
scenario is a scenario, a revision a revision, an invocation an invocation.
The human and the agent should not need translation tables. When the driving
agent speaks about the work in the API's vocabulary, it is speaking the
language the user also reads and understands—so the conversation, the shared
screen and the recorded evidence all line up. Divergent structure or invented
UI-only labels silently sever that shared language.

**The caller is the judge.** The harness supplies evidence, not a verdict or a
fix. The caller may be a human or an agent acting with the human's intent. An
optional `reason` frames the investigation; `design_goals` documents intent.
Neither is an oracle enforced by the harness. Traces are useful even when
nothing is obviously wrong. The tool is the loop body; the user is the loop.

## The API/UI fidelity contract

### The API must explain itself

An agent reading only `openapi.json` must be able to use the product correctly
without repository access. Endpoint and schema descriptions must explain
concepts, relationships, lifecycle rules and recovery paths, not merely repeat
field names. Explain what a scenario is and why to probe it, what a run records,
and how to inspect and annotate the evidence.

The spec is generated from handler/type `utoipa` annotations. Whenever endpoints,
request/response shapes or their descriptions change, run
`scripts/dump-openapi.sh` and commit `openapi.json` and `API.md` with the change.
The spec is also a prompt: validate its words with the dogfooding process below.

### The UI must be a faithful abstraction

**Faithful does not mean literal JSON. Abstraction does not mean omission.**
The UI should make the API's information easier for a human to understand while
preserving its meaning, relationships and inspectable detail. Information parity
does not require a widget for each endpoint or every field expanded at once.
It does require that the human can find the same facts the agent is discussing.
Every exposed fact should earn its place by informing a real human decision;
parity means both interfaces see the same necessary set, not that either one
quietly holds more.

- **Preserve the chat-like conversation interface.** It makes turns, speakers,
  reasoning, tool requests and responses intelligible. Replacing it with a JSON
  wall is a regression, not a fidelity improvement. Rich, domain-aware rendering
  is valuable when it faithfully represents the recorded evidence.
- **Structure and naming follow the API.** Mirror the API's grouping, nesting
  and relationship between facts, and use its names for them. Do not reorganize
  evidence into UI-only categories or rename a field into a friendlier synonym;
  a human and an agent must be able to name the same thing the same way.
- **Good typography, hierarchy and interaction are features.** Structure evidence
  with a small, intended vocabulary of headings, prose, labels and code so a
  human can scan, compare and drill in. Reading order and disclosure are design
  decisions that carry meaning; get them right rather than relying on the data
  being technically present.
- **Every recorded property must remain inspectable.** A hand-maintained list of
  “interesting” fields must not silently drop the rest of a fact the human needs.
  Specialized views should account for what they render and expose remaining
  properties through a shared, human-readable structural renderer in the relevant
  detail. New fields must remain discoverable without waiting for a bespoke
  component to be updated. This governs what is visible *within* facts worth
  exposing; whether a fact should be recorded or returned at all is the parity
  question above—if the human doesn't need it, neither does the agent.
- **Generic rendering is an implementation principle, not a visual style.**
  Reuse consistent object/key-value, array/list and scalar presentations. Keep
  multiline prose readable, nested structure navigable, and code recognizable
  as code. Do not equate “render JSON” with `JSON.stringify` in a monospace
  `<pre>`. Raw JSON and downloads are useful verification/export tools; they are
  not a substitute for a faithful main interface.
- **Do not invent semantics through presentation.** Arbitrary keys in `params`
  have no special status. A key named `model`, `prompt` or `extract` is not a
  privileged input merely because its name looks familiar. Use a small,
  consistent typography vocabulary based on genuine roles—headings, prose,
  labels, code—not ad hoc styling for individual property names.
- **Preserve scope and identity.** Submitted program/parameters/scenario/limits
  are investigation inputs. Prompts, models, arguments and effective budgets
  chosen during execution belong to their particular calls. Do not promote
  runtime values into original inputs or blur global budgets with local limits.
  A human and an agent must be able to identify the same scenario revision,
  investigation, invocation, turn and exchange.
- **Summaries must not change the story.** Distinguish requested and effective
  settings, limits and consumption, intermediate outputs and program return,
  missing values and zero/false/empty values, running and terminal evidence.
  `done` does not mean a final answer; `computed` does not mean faithful.
  Unknown cost is not zero. Preserve failures and cutoffs rather than presenting
  a plausible success story from the last available text.
- **Use progressive disclosure, not information loss.** Keep compact navigation
  and one selected detail for long executions. A hundred calls must not mount a
  hundred prominent prompts or transcripts. Repeated names remain distinct;
  polling must not steal selection, reset disclosures or discard editing drafts.
  Full values must be reachable without ellipsis-only truncation.
- **Share state, not UI-only truth.** API-side changes must become visible in
  the UI. Human edits use the same API rules as agent edits. Browsing filters
  and selections are presentation state, not hidden changes to the experiment
  or the comparison cohort. Share links must not contain authentication tokens.

### Validate communication, not just rendering

For an API or UI change, ask: **could the human and the agent, using their own
interfaces, point to the same evidence and reach the same understanding?**

Test representative records against their rendered views, including unfamiliar
properties nested in controls, budgets, outputs, turns and provenance. Verify
that local limits such as 1 versus 3 steps are discoverable in the correct call,
not merely present somewhere in a full-record download. Check that the UI's
structure and vocabulary still match the API's after the change, so an agent
discussing an invocation or a revision is describing what the human is looking
at. Cover null, zero, false, empty values, partial failures and live updates.
Preserve stable selection and bounded rendering with a 100-call fixture; these
need no paid model calls.

Inspect the populated UI visually on desktop/mobile and in light/dark themes.
Check actual reading order, typography, labels and interaction—not just DOM
existence or screenshots of an empty state. Keep chat readability and test that
untrusted evidence is rendered as content, never executable markup. Automated
checks and expert review are not independent first-time-human usability studies.

## Thin harness, explicit responsibility

**Deterministic bookkeeping belongs in code; semantic work belongs to the
model or the caller.** The harness routes messages, validates shapes, applies
state patches, enforces budgets, records evidence, computes diffs and performs
comparison arithmetic. Scenarios and implementations are authored outside the
harness. The simulator resolves inputs and renders world responses. The caller
judges simulation quality and application behavior.

We chose LLMs knowing they are imperfect. A simulation may contradict the
narrative. The answer is transparency and caller iteration, not another system
that tries to compile or enforce natural-language intent. Do not build a
narrative DSL, materialized environment, render cache or in-harness consistency
judge. Caller-authored Lua can supply exact behavior, but its execution is not
proof of fidelity either.

There is no in-harness verdict, automatic fix or scenario-generation endpoint.
Do not reintroduce generation-on-submit or an optional scenario whose absence
means “generate one.” Earlier judge and fix-suggestion systems were removed:
they encoded a fragile oracle and displaced the caller's richer intent.

Every active phase must be observable in the API and UI. Show `resolving_inputs`
or `put_loop` during that LLM work, and orchestration when applicable—not only
an undifferentiated “running.” There is no model-driven preparation/authoring
phase during an investigation.

## Scenarios: reusable world specifications

A scenario is `(world, input_domain, user_message, simulator_notes, tools[],
simulation settings)` plus its initial workspace. It is a narrative world
specification, not an instantiated environment. Closed worlds can have total
completeness assertions; open worlds require scoped ones. The simulator lazily
renders concrete responses from that specification.

Inputs are **described, not supplied**. `input_domain` describes the value space,
semantics and preconditions/trust contracts of each `{{variable}}`. The simulator
samples concrete values, fills the template and records `resolved_inputs`.
This is the property-based-testing move: describe the domain, sample it.

Author worlds with four explicit elements, all visible to simulator and caller:

1. **Inventory:** what exists and where, covering every query type tools allow.
2. **Facts:** positive and negative; say what does not exist and never happens.
   Models otherwise tend to invent positive content.
3. **Completeness:** “these are ALL entry points” for a closed world, or “these
   are the relevant results on this topic” for an open one.
4. **Rendering rules:** refuse out-of-inventory queries, introduce no facts
   through filler, and never contradict the stated facts.

Size worlds to the step budget; a small explored world beats a large unfinished
one. Vary the corpus rather than proving the same thing in same-shaped worlds.

### Lifecycle and simulation quality

Register a definition once with `POST /api/scenarios`. Investigations reference
`scenario_id`, never an index, and report the exact `scenario_revision` and
`scenario_definition_hash`, plus the narrative by value. Core's
`scenario::ScenarioStore` owns these rules, not HTTP handlers:

- Editable only while unreferenced; stale edits are refused.
- Pinned when an investigation is accepted; finished runs keep pinning it.
- Deletion refused while dependents run, and without `cascade=true` when
  dependents exist.
- Forks share an immutable workspace seed rather than requiring re-upload.
  Corrections create a new revision identifying what they correct; old evidence
  must never silently acquire a new definition.

Develop and test a world before spending investigations. Simulation probes
(`POST /api/scenarios/{id}/simulations`) execute caller-submitted tool calls
through the same engine as investigations, retaining responses and provenance,
without requiring a local Lua toolchain. The consumer judges their fidelity.
If simulation is inadequate, the caller sharpens or forks the scenario and
re-probes. No automatic repair, fixture system or hidden fidelity judge.

## Investigations and evidence

Each investigation runs one program against one scenario. Results use
`result.trace` or `result.failure`, with flat `progress`. Agent invocations are
stages inside that execution, not independent investigations; workflow evidence
references ranges in the flat turns array. Repetition means separate
investigations, not nested trace arrays or an embedded `samples` control.
Workspace reuse across investigations and multi-submit convenience are deferred.

Prefer `GET /api/investigations/{id}/evidence` when reading/grading. It contains
complete execution and actual tool responses/provenance without duplicated
terminal progress. `workspace_ops` supports provenance; it is not a fidelity
verdict. Neither plausible output nor Lua `computed` proves correct simulation.

Preserve original budgets, effective stage budgets, `execution.stop_reason`,
counters and monotonic elapsed/phase timing. Failed and capped executions retain
partial evidence and charged work. `done` means evidence recorded, not that the
application delivered a usable answer.

Caller-owned `assessment` stores summary, rubric and evidence references beside
numeric grades. PATCH replaces/clears it atomically with the maps. No mandatory
grade, automatic stage-grade combination or harness judgment. Do not add
`review_status` as a side effect of UI work; that is separately scoped work.

System `simulation_backend`, `step_budget` and `token_budget` attributes support
explicit comparisons. Defaults do not infer the intended experimental cohort.
Model catalog availability proves neither generation readiness nor balance;
never make a hidden charged readiness probe. See `docs/design/evidence-first.md`.

## Lua orchestration: the application under test

The program belongs to the investigation, **never the scenario**. All
investigations use Lua, including the default single-agent program. `params` is
arbitrary JSON with no privileged keys; only the program decides what to pass
to `ctx.run_agent` or `ctx.call_tool`. Default-program parameter names are
conventions, not harness magic. Do not replace this with a DAG DSL.

`ctx.run_agent` runs a complete agent conversation. Each invocation gets fresh
messages; all calls share one scenario simulation session/world/workspace.
Direct orchestration tool calls use the same engine and provenance as agent
tool calls. Source, parameters, literal handoffs, settings, effective budgets,
turn ranges, outputs, failures and running state are evidence.

The host owns immutable evidence, whole-investigation accounting, budgets and
sandbox limits. Lua cannot erase charged work or bypass exhaustion through
retries or `pcall`. Application Lua cannot inspect world truth, tool source or
simulator internals. Orchestration errors never invoke simulator fallback.

`workflow.output` is the application's returned value, not automatically the
last agent completion. Absent output and an empty final string are distinct.
Price usage per actual model; never apply a nominal model's price to tokens
from multiple models. Unknown pricing must remain unavailable.
See `docs/workflow-api.md`.

## Lua tool handlers: simulator-private implementations

A scenario tool's optional `lua_source` supplies its implementation.
`simulation.lua` sets resource limits; it is not an enable switch.
`conversation_controls.lua_simulation` is gone. The harness never generates,
repairs or rewrites handlers, creates fallback modules, or specializes source
during a fallback. Keep helper code local to each chunk; no Lua package system.

A chunk returns `function(args, ctx)`, which returns `{response=...}` and, for
write tools, `state_patch=...` (use `{}` even for workspace-only writes). Read
tools omit the patch or use an empty object. `ctx.workspace` provides bounded
`list_dir`/`read`/`grep`/`write` capabilities for exact uploaded bytes.

Missing implementations and `PleaseSimulateException("reason")` delegate that
one call to the simulator LLM. Runtime errors and resource limits also delegate,
with distinct evidence. Staged workspace writes are rolled back before fallback.
Computed and rendered responses enter the same conversation. Each attempt
records tool name, exact `source_hash`, outcome and discarded operations.

Tool `lua_source` is simulator-private and may contain ground truth. It must
never appear in the contract the application under test sees. The reserved
`.prompt-explore` namespace remains rejected in uploads and hidden from
application Lua workspace access; removing that reservation is separate work.

Limits are explicit, validated and hard-ceilinged. Workspace results are
byte-bounded before construction. The VM exposes existing in-memory operations,
not IO/network access. Randomness/time capabilities are deferred. CPU deadlines
are cooperative, not OS isolation; native operations cannot be preempted mid-call.

The public handler reference is `GET /docs/lua` (`docs/lua-api.md`). Keep its
contract reachable from OpenAPI through `LuaWorkspaceCapability` and
`lua_source` descriptions; spec-only callers cannot read this repository.

## Live grouped Pareto frontier

All investigations in memory are candidates. There is no server-side selection
list. `POST /api/frontier` groups by attributes (default `put_model`,
`put_thinking`, `prompt_hash`) and averages requested axes over completed
investigations with **every** requested value. All coordinates use the same
cohort, equally weighted per investigation. Browsing filters never change
frontier candidacy. The caller owns comparability and grading; the harness owns
only grouping and arithmetic.

- The map is `attributes`, not `tags`; no compatibility alias. System attributes
  (resolved settings, content hashes, pinned scenario identity) are immutable.
  Custom attributes are editable. `label` changes group identity only when
  explicitly selected for grouping.
- Display groups as slash-separated attribute values in `group_by` order, not
  opaque group hashes. Show caller-owned values completely. Model names may use
  basenames; hashes use labeled eight-character prefixes, with full values
  retained and inspectable in `attributes`.
- Missing grouping attributes form null-valued groups. Missing grades, running
  or failed jobs and unavailable axes remain an explicit per-group backlog,
  never silently dropped or made a whole-request error.
- Groups without contributors are pending, without coordinates. Any exclusions
  make a group preliminary. Preliminary groups participate in dominance, with
  faded markers and dashed outer rings. Poll/redraw as jobs and annotations change.
- SVG labels belong in a right-side PCA-ordered legend using normalized rendered
  coordinates, not beside dots. Link marker/legend hover and keyboard focus.
  The staircase is the observed dominated-region boundary: turn at the current
  point, never interpolate. Always include a light, non-interactive fill.

## Architecture and engineering conventions

Cargo workspace:

- `core/`: all business logic, usable standalone as a library/CLI/examples.
  `model/`, `llm/`, `simulate/`, `scenario/` and `generate/` hold the model layer,
  client abstraction, simulation, reusable registry/probes and orchestration.
  There is no judge module.
- `server/`: thin axum HTTP wrapper and web UI. No business logic here. The job
  store is deliberately **in memory**: jobs and annotations disappear on restart.
  Do not casually add persistence; durability needs an explicit design decision.

All LLM access goes through `LlmClient`; runtime layers do not depend on a
concrete provider. Tests use scripted `MockLlmClient` responses, deterministic
and without network. Conversation controls—temperature, token limits, retries,
turn budgets—need named, documented overrides at the appropriate boundary,
with explicit defaults, not buried magic constants.

When updating a populated local server, export evidence before restart. An
export is not restoration of the live workspace: say clearly when IDs and
in-memory runs are lost. Do not silently substitute fresh runs for old evidence.
Keep one local instance unless the operator requests otherwise. Avoid unrelated
UI, API, persistence or release changes while addressing a scoped task.

## Validation and dogfooding

**When changing a model-facing prompt, run prompt-explore against itself.**
Use the same scenarios before and after, read the traces, and record the finding
in the commit message. Reasoning that a prompt is better is not validation.
If changing the simulator, check that investigations still produce meaningful
evidence. Preserve failures and regressions; do not claim a clean pass from
mixed results.

**OpenAPI wording is in scope.** Feed the spec verbatim to an LLM as its only
manual. Use matched realistic caller probes before/after and inspect actual
endpoint choices, request shapes, recovery and evidence reading. Invented or
missed affordances are spec bugs: fix the manual, not the prober's model.

Probe framing itself needs iteration. Asking “how would you grade traces?”
mostly gets polished plans. A better probe puts a coding agent with bash access
in a real situation: it triggered an investigation for a goal, the job is done,
the response is ~100 KB, and it must now grade the evidence. Observe whether it
reads traces, navigates with jq, substitutes keyword grep, or actually PATCHes
an assessment. Test behavior, not declarations of intended behavior.

If the probe world simulates API tool access, embed the real spec in that world
and pin rendered responses to it. Otherwise the simulator invents API behavior
and confounds the experiment. Prior A/B probes without the spec invented
read-only PATCH errors (0/3 grades recorded); with it, PATCH and re-GET followed
the contract. See `docs/dogfood/` for retained trials and limitations.

For UI work, apply the API/UI fidelity checks above. Browser tests alone cannot
establish that humans understand the evidence; inspect the real reading
experience and distinguish expert review from independent usability testing.

## Build, test and run

```bash
cargo test
cargo build -p prompt-explore-server
ZAI_API_KEY=... cargo run -p prompt-explore-server
ZAI_API_KEY=... cargo run --example investigate_live
```

The server defaults to `127.0.0.1:8080`. Live calls use `ProviderClient` through
the `LlmClient` trait and the `genai` provider library. Provider selection is per
model namespace: `zai_coding::glm-5.2` (default, `ZAI_API_KEY`),
`open_router::<model>` (`OPEN_ROUTER_API_KEY`), or `bedrock_sigv4::<model-id>`
(default AWS credential chain; `aws sso login` works). Bare model names use
`PROMPT_EXPLORE_PROVIDER`, default `zai`.

## Releases

Releases are tag-driven. Pushing `v*` triggers `.github/workflows/release.yml`,
which builds and packages Linux x86_64/ARM64 (static musl), macOS x86_64/ARM64,
and Windows x86_64, then attaches archives to the GitHub Release. Do not
hand-build or hand-upload release artifacts. No provider keys are needed at
build time; publishing uses GitHub's injected `GITHUB_TOKEN` with
`contents: write`.

1. **Confirm the version with the operator before bumping or committing.** Ask
   patch/minor/major or an explicit number; do not infer breaking-change intent.
   Proceed without asking only if the operator already named the version.
2. Start from green `main` and a clean working tree. Run `cargo test`. Bump the
   single workspace package version in `Cargo.toml`; update generated versioned
   artifacts and the lockfile as needed.
3. Commit, tag that commit and push both:
   ```bash
   git commit -am "Bump version to X.Y.Z"
   git tag vX.Y.Z
   git push origin main
   git push origin vX.Y.Z
   ```
4. Watch all five build jobs and publication. **Write real release notes** after
   all assets are attached. Review the full diff since the previous tag; group
   changes by user-facing features, fixes and caveats, with their why. A generated
   commit list is not enough. Apply with `gh release edit <tag> --notes-file <file>`.
   Commit/push associated repository-doc changes too; leave nothing dangling.

For a first or risky cut, ship as prerelease, verify downloads and binaries on
each platform, then promote to Latest. Releases can be edited or deleted and
re-cut; do not leave known-bad assets as someone's first impression.

Use `workflow_dispatch` to dry-run matrix/workflow changes without publishing:
it builds all targets as workflow artifacts but skips the tag-only publish step.
Every third-party action is pinned to an immutable commit SHA with its original
ref in a trailing comment. Updating a SHA is deliberate; never replace it with
a moving `@vN` ref.
