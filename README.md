# prompt-explore

Designing agentic prompts is hard - LLMs are notoriously bad at predicting how well a prompt will work in practice.

If your workflow involves your agent writing or updating prompts, this tool is for you: **point your coding agent at this readme, ask it to download the latest release and demo it to you. Should be ready and running in a minute.** Needs an openrouter key, z.ai plan key, baseten key, vertex access or aws bedrock access.

Being able to quickly try out a prompt on a wide range of inputs is a basic requirement for optimizing a prompt effectively. But tool calling prompts resist easy experimentation because traditional, deterministic mocking doesn’t work - you can’t, ahead-of-time, mock the tool call a model under test chooses! You can’t quickly ask: „What if we added this one more tool?“

**The solution:** This tool executes _any_ agentic prompt (the 'prompt under test', PUT) with _any_ tools that it needs by pairing it up with a second LLM, the simulator. The simulator's job is simple: to mock a response for every tool call, live as the calls come in.

A user of `prompt-explore` can steer the simulator's behaviour by controlling the world the simulator is operating in.

**Example:** if you're testing a user support agent prompt, you may want to tell the simulator that the user is a premium subscriber with a long positive shopping history, but the last three shipments were cancelled. The simulator picks that context up and mocks responses accordingly.

The tool is 100% sandboxed, no tool calls can ever reach the outside, no hard drive or IO access to any tools. This makes it easy and secure to run many scenarios in parallel.

## One investigation, one conversation

Each investigation runs **one authored scenario** against one prompt under test,
with one optional workspace upload. Submit separate investigations concurrently
for different worlds/workspaces or repeated runs; use attributes to group them.
There is no batch or sample-count field. Repeating a scenario samples its inputs
and simulation anew, not just the PUT's response to fixed inputs.

The request uses `scenario` (not `scenarios`). Inspect the complete conversation
at `result.trace`, or the failure at `result.failure`; live and failed partial
evidence remains in the flat `progress` object. The job's `phase` reports
`resolving_inputs`, `preparing_tools`, or `put_loop`.

## Multi-dimensional prompt optimization (grades + Pareto frontier)

Optimizing a prompt is never only about correctness — you also care about
cost, tone, writing style, repeatability, … prompt-explore
supports this without ever judging for you:

- **Measured axes** are harness-computed on every run and cannot be
  graded: `put_/sim_{input,output,cache_read}_tokens`, `put_/sim_cost_usd`
  (when the model catalog prices the model), and
  `steps_per_trace_{avg,min,max,stdev}`. Their better-direction is baked
  in (tokens lower, cache-read higher, cost lower, steps lower).
- **Judged axes** are yours: PATCH numeric grades with free-form axis
  names onto an investigation. The harness stores them and never
  interprets them.
- **Attributes group investigations into points.** Each point represents one unique
  combination of your chosen attribute values, averaged across its completed, fully
  graded investigations. Run the same prompt against several workspaces to
  contribute to one point. Every investigation is a candidate; there is no
  selection/filter list. Delete investigations you do not want included.
- `POST /api/frontier` takes `group_by` attribute names and `axes` with directions
  (`"better": "lower" | "higher"`). `?format=json` supports N axes;
  `?format=svg` renders exactly two. **Up-and-right is always better**:
  lower-is-better axes are reversed. Filled dots are non-dominated; hollow
  dots are dominated. Labels live in a PCA-ordered legend to the right—not
  beside the dots—and flow into additional columns as groups grow. Hovering or
  keyboard-focusing a legend entry highlights its dot and vice versa. The
  corrected axis-aligned staircase lightly shades the region dominated by
  observed frontier points at all times. The frontier is relative to the
  chosen axes, not a verdict.
- **The plot is live.** The UI refreshes as jobs finish, grades/attributes change,
  or jobs are deleted. Groups with running or excluded members are preliminary
  (faded dots with dashed outer rings). Groups with no usable results are
  pending, not given invented coordinates. Every excluded investigation and
  its missing grades remain visible in the API and UI as a grading backlog.

Attributes are string-valued. `put_model`, `sim_model`, `put_thinking`,
`sim_thinking`, `prompt_hash`, and `workspace_hash` are recorded automatically
and cannot be overwritten or deleted. `label` is editable and displayed in
the UI; other custom attributes are editable too. Renaming a label leaves the default
grouping unchanged; explicitly grouping by `label` makes it an identity key
like any other selected attribute. Model attributes use resolved names;
missing thinking settings are `provider_default` (different from explicit
`none`). Prompt hashes exclude the cosmetic PUT id; workspace hashes describe
extracted paths and contents, not zip metadata.

The UI groups investigation cards by the same selected attributes as the plot.
Hover/focus links a point or legend entry to its card group; activate it to scroll
there. For API browsing, filter the list with exact attribute matches:

```bash
curl -sS -G http://127.0.0.1:8080/api/investigations \
  --data-urlencode 'attributes={"campaign":"support"}'
```

Multiple pairs are ANDed; missing attributes do not match. This filters only
the list response, never frontier candidacy.

Everything remains in memory: restarting loses investigations, attributes, and grades.
Groups and their frontier are computed on demand; an API caller polls the same
POST to refresh. Group ids remain stable when membership or grades change.

### Example: four variants of a cancel-bot prompt

Run the whole flow against a seeded loopback server — no provider keys
needed, grading and the frontier are LLM-independent:

```
$ prompt-explore-server --demo-frontier
```

After reading the traces, grade the soft axes and optionally label the run.
Both maps merge per key; `null` deletes an editable entry. The response echoes
both full maps:

```
$ curl -X PATCH 'http://127.0.0.1:8099/api/investigations/v2-warm' \
    -H 'content-type: application/json' \
    -d '{"grades": {"tone_of_voice": 0.85}, "attributes": {"label": "Warm variant"}}'
```

Custom attributes can also be supplied in the `attributes` object when creating
an investigation. System-owned attributes are read-only even at creation time.
There is deliberately no `tags` compatibility alias: unknown fields are rejected.

Reserved axes are harness-computed, so grading one is rejected with the
fix named:

```
$ curl -X PATCH .../api/investigations/v1-terse -d '{"grades": {"put_cost_usd": 3.0}}'
{
  "error": "grades_invalid",
  "problems": [ { "axis": "put_cost_usd", "reason": "reserved_axis_name",
      "detail": "'put_cost_usd' is a reserved measured axis (better: lower); measured axes are computed by the harness and cannot be graded — pick a different name" } ]
}
```

Then request one point per model/thinking/prompt combination (the default
`group_by` if omitted), across **all** investigations:

```
$ curl -X POST 'http://127.0.0.1:8099/api/frontier?format=json' \
    -H 'content-type: application/json' -d '{
      "group_by": ["put_model", "put_thinking", "prompt_hash"],
      "axes": [{"name": "put_output_tokens", "better": "lower"},
               {"name": "tone_of_voice", "better": "higher"}] }'
```

Each returned point carries its grouping `attributes`, a stable `id`, and a
compact slash-separated value label in `group_by` order (for example
`gpt-5.6-luna/low/prompt-a1b2c3d4`), plus all member `investigations`, the `included` ids used for **every** coordinate, and an
`excluded` backlog. `values` contains arithmetic means, not totals across the
group. `on_frontier` and `dominated_by` describe dominance between **group ids**.
Ties dominate nothing. `group_by: []` means one group; an absent grouping attribute
has a `null` value, distinct from any string.

An exclusion names its `investigation`, `status` (`running`, `failed`,
`awaiting_grades`, or `unavailable`), `missing_grades`, and `missing_axes`.
PATCH missing grades after reading that investigation's traces, then repeat
the same frontier request. Missing grades and unfinished jobs do **not** make
the whole plot fail. A pending group has `values: null` and
`on_frontier: null`; a group with any exclusions is `preliminary: true`.
Preliminary points participate in dominance using their current means.

The same request with `?format=svg` renders the plot and pending/backlog
information. The UI shows membership and the grading backlog alongside it.

**Aggregation is deliberately simple:** every included investigation has equal
weight. All requested coordinates use the same complete cohort. Each
investigation contributes one conversation's measurements and caller grades.
Keep scenarios, budgets, grading scales, and simulator settings comparable;
the harness surfaces membership but does not judge comparability.

**API migration:** investigations now require singular `scenario`; read
`result.trace` or `result.failure` and flat `progress`. Split old scenario arrays
into separate submissions. See [the single-conversation contract](docs/design/single-conversation.md).
The former `investigations` selection field on frontier
requests is replaced by `group_by`; old selection requests are rejected rather
than silently broadened to all jobs. Groups with missing data now appear in a
successful response, not a missing-grade 422. Invalid axes/grouping still
return typed validation errors.

## Server architecture

The tool functions as a server with an endpoint that serves a thoroughly documented openapi spec. Simply point your coding agent at 127.0.0.1:8080/openapi.json and it will know how to use this.

## Code

This entire repo is a vibe coded server (only the README is mostly written hand-written). There was no security audit. It comes without warranty.

### Authentication (optional)

By default the server binds to `127.0.0.1:8080` (loopback-only, reachable only
from this machine) and runs with no auth. A non-loopback bind
(`PROMPT_EXPLORE_ADDR=0.0.0.0:8080`) is refused unless you set
`PROMPT_EXPLORE_ALLOW_INSECURE_PUBLIC=1`, because over plain HTTP the bearer
token and all traces travel in cleartext. If you do expose it, set
`PROMPT_EXPLORE_API_TOKEN` too: `POST /api/investigations` spends your provider
credits. When a token is set, every `/api/*` route (except the OpenAPI spec)
requires an `Authorization: Bearer <token>` header. The web UI prompts for the
token and stores it in localStorage.

## Supported APIs

 - AWS Bedrock
 - Vertex API
 - Baseten
 - OpenRouter
 - z.ai coding subscription
 - send a pr or feature request if you'd like more.

## How to try this out

### 1. Get the server binary

**Option A — download a release (recommended; no Rust toolchain needed).**
Grab the archive for your platform from the
[Releases page](https://github.com/kaeluka/prompt-explore/releases), extract
it, and run it directly:

```
tar -xzf prompt-explore-server-<your-target>.tar.gz   # .zip on windows
./prompt-explore-server --version
```

Prebuilt targets: linux x86_64/ARM64 (musl — fully static, runs on any
Linux), macOS x86_64/ARM64, windows x86_64.

**Option B — build from source** (needs a Rust toolchain):

```
$ cargo build --release
$ target/release/prompt-explore-server --version
```

Whichever path you take, `--help` prints usage and the environment variables
(abridged example):

```
$ prompt-explore-server --help
prompt-explore-server 0.4.2

Property-based testing for agent behavior. HTTP API + web UI.

USAGE:
    prompt-explore-server [OPTIONS]

OPTIONS:
    --dump-openapi    Print the OpenAPI spec as JSON and exit
    --demo-frontier   Run the grades + Pareto-frontier demo against a live
                      loopback server (seeded with a representative 4-variant
                      campaign; no provider keys needed — grading and the
                      frontier are LLM-independent), print the HTTP transcript,
                      and exit
    -h, --help        Print this help message and exit
    -v, --version     Print version and exit

ENVIRONMENT:
    PROMPT_EXPLORE_PROVIDER  Which provider runs the LLM calls (default: zai).
                           zai | zai_standard | openrouter | bedrock | baseten | gemini
    ZAI_API_KEY            API key for zai / zai_standard (coding-plan default).
    OPEN_ROUTER_API_KEY    API key for openrouter.
    bedrock uses the default AWS credential chain (aws sso login, profiles, IMDS).
    gemini uses GCP Application Default Credentials (gcloud auth application-default
                           login). Project: VERTEX_PROJECT_ID or gcloud config;
                           region: VERTEX_LOCATION (default: global).
    BASETEN_API_KEY      API key for baseten (OpenAI-compatible).
    BASETEN_ENDPOINT     Baseten endpoint (default: https://inference.baseten.co/v1/).
    PROMPT_EXPLORE_ADDR    Bind address (default: 127.0.0.1:8080, loopback-only).
    PROMPT_EXPLORE_API_TOKEN  Optional bearer token. When set, every /api/* route
                           (except the OpenAPI spec) requires an
                           `Authorization: Bearer <token>` header.
                           Empty or unset = open mode (no auth).
    PROMPT_EXPLORE_ALLOW_INSECURE_PUBLIC
                           Set to 1 to allow a non-loopback bind over plain HTTP
                           (the bearer token and all traces travel in cleartext).
    PROMPT_EXPLORE_MAX_WORKSPACE_TURNS
                           Maximum workspace tool calls the simulator may make per
                           response before being nudged to produce a final answer
                           (default: 250). Raise if your scenarios have large
                           workspaces that need more lookups per tool response.
    PROMPT_EXPLORE_WORKSPACE_COMPRESSED_LIMIT
                           Maximum size (bytes) of uploaded workspace .zip files
                           (default: 52428800 = 50 MB).
    PROMPT_EXPLORE_WORKSPACE_DECOMPRESSED_LIMIT
                           Maximum total decompressed size (bytes) of workspace
                           contents (default: 524288000 = 500 MB).
```

### Retry controls

Long investigations should survive transient provider failures. Each PUT or
simulator completion gets **20 retries after its initial attempt** by default:
transient 429 rate limits, HTTP 408/server errors (except permanent 501/505),
connection/timeouts, interrupted responses, and malformed HTTP response
bodies. Authentication, request-validation, and billing/quota errors still
fail promptly. Only the failed completion is retried, with the same request;
completed tool calls and scenarios are not replayed.

Process-level overrides:

- `PROMPT_EXPLORE_MAX_RETRIES=20` — retries per completion; `0` disables them.
- `PROMPT_EXPLORE_RETRY_BASE_DELAY_MS=5000` — linear waits of 5s, 10s, …, 100s.
- `PROMPT_EXPLORE_RETRY_JITTER_PERCENT=10` — adds up to 10% positive jitter.

A longer provider `Retry-After` (seconds or HTTP-date) or `retry-after-ms`
extends the wait. Retry number, delay, model, and failure category are logged
without dumping request bodies or headers. Exhausting the default budget
can take roughly 18–19 minutes in backoff alone, plus request time; retries
are generous, not unlimited. A lost response may still have been generated
and billed by the provider, so actual costs can exceed reported usage.

Malformed **simulator content** has a separate repair budget: **20 total
attempts, including the initial reply**, per JSON answer. Empty replies,
invalid JSON, and schema mismatches are retried in the same conversation;
repair feedback includes the parser diagnostic and location when available.
Override it per investigation with
`"conversation_controls": {"sim_max_repair_attempts": 30}`. The job view's
`conversation_controls.sim_max_repair_attempts` reports the resolved value.
This setting does not change HTTP/transport retries. Failed investigations
are not automatically resubmitted when either budget is exhausted.

### Experimental Lua simulation

Add `"conversation_controls": {"lua_simulation": {}}` to an investigation to try
hybrid execution. This backend is experimental and may change while its
semantics and speed are assessed; it is opt-in, and omitting the option (or
sending `null`) keeps the existing LLM-only behavior. The simulator can
specialize `.prompt-explore/tools.lua`
using its workspace tools. Handlers compute suitable inputs and call
`PleaseSimulateException("reason")` for others. Computed and LLM-rendered
responses share one conversation; failed/delegated Lua writes are rolled back.

The UI shows the generated source, every revision, and setup work beside the
resolved inputs. Exchanges identify Lua computation, intentional delegation,
or Lua errors followed by LLM recovery. Lua controls have documented hard
ceilings, and workspace result construction is byte-bounded before data enters
the VM. **Generated code is unverified:** it can run successfully and still
contradict the world. See
[the prototype notes, limits, and live findings](docs/lua-simulation.md).

### 2. Setup with your coding agent

Tell your coding agent to
1. read the README. Place an api key for one of the supported providers in a local file and point the agent at the file (or log in using the `aws` or `gcloud` clis).
2. ask the agent to start the server and read the openapi specs.
3. ask the agent to try the tool out, while you watch the output in the web ui at http://127.0.0.1:8080

If you started the server with `PROMPT_EXPLORE_API_TOKEN` set, give the token to
your agent too (it goes in an `Authorization: Bearer <token>` header on `/api/*`
calls), and enter the same token in the web UI when prompted.

### 3. Example: GPT-5.6 Luna on AWS Bedrock (0.4.0+)

With an existing AWS SSO profile that has Bedrock model-invocation access,
log in and start the server (replace `your-profile` with your profile name):

```bash
aws sso login --profile your-profile
AWS_PROFILE=your-profile AWS_REGION=us-east-1 \
  PROMPT_EXPLORE_PROVIDER=bedrock ./prompt-explore-server
```

Existing `aws login` credentials, workload roles, and other sources in the
AWS credential chain also work; no permanent access key is needed. Catalog
access alone does not guarantee invocation access.

In another terminal, submit a small inventory scenario. Both roles explicitly
select Luna's US inference profile: the PUT uses `high` reasoning, while the
simulator uses `none`. Model and thinking settings are independent per role.
Only the LLM calls reach AWS; `lookup_stock` is simulated, not a real tool.

```bash
curl --fail-with-body -sS http://127.0.0.1:8080/api/investigations \
  -H 'Content-Type: application/json' \
  --data-binary @- <<'JSON'
{
  "investigation": {
    "reason": "Check that Luna looks up stock rather than inventing availability.",
    "budget": {"max_steps_per_trace": 3}
  },
  "put": {
    "id": "luna-stock-check",
    "template": "You are an inventory assistant. Always look up stock before answering availability questions. Never invent stock counts.",
    "design_goals": "Use the lookup result and report availability accurately.",
    "tools": [{
      "name": "lookup_stock",
      "description": "Return the stock count for one SKU.",
      "parameters": {
        "type": "object",
        "properties": {"sku": {"type": "string"}},
        "required": ["sku"],
        "additionalProperties": false
      },
      "side_effect": "read"
    }]
  },
  "put_model": "bedrock_sigv4::us.openai.gpt-5.6-luna",
  "sim_model": "bedrock_sigv4::us.openai.gpt-5.6-luna",
  "put_thinking_level": "high",
  "sim_thinking_level": "none",
  "conversation_controls": {"put_max_tokens": 2048, "sim_max_tokens": 2048},
  "scenario": {
    "world": "SKU-7 is a Solar Lantern with stock 3. This is the complete inventory; no other SKUs exist and stock never changes. lookup_stock returns the requested SKU and its stock count, or an unknown-SKU error. Never invent items or contradict these facts.",
    "input_domain": {},
    "user_message": "Is SKU-7 in stock?"
  }
}
JSON
```

The response is `{"id":"..."}`. Poll with the returned ID until `status`
is `done` or `failed`, or watch the run in the web UI:

```bash
curl -sS http://127.0.0.1:8080/api/investigations/REPLACE_WITH_ID
```

Read `result.trace.turns[]` for the model output and tool exchanges. A failed
job exposes `result.failure`; inspect `progress.turns`, `progress.resolved_inputs`,
and `progress.simulation_program` for the evidence collected before it failed.
The caller judges the trace; the harness does not grade whether Luna behaved correctly.
If server authentication is enabled, add your `Authorization: Bearer ...`
header to both requests.

Luna supports `none`, `low`, `medium`, `high`, `xhigh`, and `max` on Bedrock;
`minimal` is rejected. Omitting a thinking field keeps the provider default,
which is not the same as `none`. For comparison, GPT-OSS supports only
`low`/`medium`/`high`, and Astra supports `low`/`medium`/`high`/`xhigh`/`max`
but not `none`. The harness forwards these keywords literally: unsupported
values are not downgraded and can fail during execution after POST returns
202. See [API.md](API.md) for the complete request and response contract.

The Bedrock reasoning fix is temporarily supplied by a [revision-pinned
genai fork](https://github.com/kaeluka/rust-genai/commit/849d657347429fe04753a487422ab06ab4e098db).
The pin will be removed once an upstream crates.io release includes it.

# Usage Advice

It usually is a good idea to design an experiment up front:

- Author scenarios for evaluation. Make sure the scenarios are appropriately varied.
- Define what success looks like:
  - for hard to define or composite qualities, consider designing a rubric to optimize for up front. Write the rubric down.
  - for classification problems: do you have reliable ground truth? Chances are, you can decide the ground truth and write a scenario to match it!
- If you can't design an experiment up front, use prompt-explore to run a few investigations and see if there's some you like more than others. _Then_, design an experiment.

Investigations can vary different parameters and it is usually a good idea to only change one at a time:

 - Prompt (and the set of tools)
 - Model
 - Scenarios

When you already have a running system: have your agent start with your current prompt. Let it read the tool implementation. Let it look at tool call logs. Give it the context it needs to faithfully model the system.

Simulation quality matters: `prompt-explore` does _not_ evaluate simulation quality for you. A certain level of errors is expected. Whether or not these errors matter depends on the conclusions you want to draw. You can not draw conclusions without checking the investigation conversations carefully. Use a dedicated subagent for simulation quality assessment.

Be wary of repeatability - a 'better' prompt may be a lucky nondeterministic output.

LLMs are very bad about suggesting prompt updates. Frontier models are not necessarily going to suggest better improvements. Use your own sense. Read suggested prompt changes critically.

 - Will they generalize?
 - Are they accumulating long lists of do-nots, rather than explain the principle?
 - Standard writing advice. Use it.